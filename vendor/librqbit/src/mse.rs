// rustorrent: Message Stream Encryption / Protocol Encryption (MSE/PE).
//
// Implements the de-facto BitTorrent obfuscation protocol used by libtorrent,
// qBittorrent, Transmission, uTorrent etc.:
// https://wiki.vuze.com/w/Message_Stream_Encryption
//
//   A->B: Ya, PadA
//   B->A: Yb, PadB
//   A->B: HASH('req1', S), HASH('req2', SKEY) xor HASH('req3', S),
//         ENCRYPT(VC, crypto_provide, len(PadC), PadC, len(IA)), ENCRYPT(IA)
//   B->A: ENCRYPT(VC, crypto_select, len(padD), padD), ENCRYPT2(payload)
//   A->B: ENCRYPT2(payload)
//
// DH uses the 768-bit MODP prime below with generator 2; RC4 keys are
// SHA1('keyA'|'keyB', S, SKEY) with the first 1024 bytes of keystream
// discarded. SKEY is the torrent's info hash.

use std::{
    io::IoSliceMut,
    pin::Pin,
    task::{Context, Poll, ready},
};

use anyhow::{Context as _, bail};
use num_bigint::BigUint;
use rand::RngExt;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::{
    type_aliases::{BoxAsyncReadVectored, BoxAsyncWrite},
    vectored_traits::AsyncReadVectored,
};
use librqbit_core::hash_id::Id20;

/// Whether to use protocol encryption.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EncryptionPolicy {
    /// Plain BitTorrent only.
    Disabled,
    /// Try encrypted connections first, fall back to plaintext; accept both.
    #[default]
    Enabled,
    /// Only encrypted connections, in both directions.
    Required,
}

const P_HEX: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A63A36210000000000090563";
const KEY_LEN: usize = 96;
const VC: [u8; 8] = [0; 8];
const CRYPTO_PLAINTEXT: u32 = 0x01;
const CRYPTO_RC4: u32 = 0x02;
const MAX_PAD: usize = 512;
const PROTOCOL_HEADER: &[u8; 20] = b"\x13BitTorrent protocol";

fn prime() -> BigUint {
    BigUint::parse_bytes(P_HEX.as_bytes(), 16).expect("valid prime")
}

struct Dh {
    private: BigUint,
    public: [u8; KEY_LEN],
}

impl Dh {
    fn new() -> Self {
        let mut x = [0u8; 20];
        rand::rng().fill(&mut x[..]);
        let private = BigUint::from_bytes_be(&x);
        let public = to_key_bytes(&BigUint::from(2u32).modpow(&private, &prime()));
        Self { private, public }
    }

    fn shared_secret(&self, other_public: &[u8]) -> [u8; KEY_LEN] {
        let y = BigUint::from_bytes_be(other_public);
        to_key_bytes(&y.modpow(&self.private, &prime()))
    }
}

fn to_key_bytes(n: &BigUint) -> [u8; KEY_LEN] {
    let b = n.to_bytes_be();
    let mut out = [0u8; KEY_LEN];
    out[KEY_LEN - b.len()..].copy_from_slice(&b);
    out
}

fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// Plain RC4, as required by the protocol (not used for anything security
/// critical; MSE is obfuscation).
#[derive(Clone)]
pub(crate) struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    fn new(key: &[u8]) -> Self {
        let mut s = [0u8; 256];
        for (i, v) in s.iter_mut().enumerate() {
            *v = i as u8;
        }
        let mut j: u8 = 0;
        for i in 0..256 {
            j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
            s.swap(i, j as usize);
        }
        let mut rc4 = Self { s, i: 0, j: 0 };
        // MSE discards the first 1024 bytes of keystream.
        let mut discard = [0u8; 1024];
        rc4.apply(&mut discard);
        rc4
    }

    fn apply(&mut self, data: &mut [u8]) {
        for b in data {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.s[self.i as usize]);
            self.s.swap(self.i as usize, self.j as usize);
            let k = self.s[(self.s[self.i as usize].wrapping_add(self.s[self.j as usize])) as usize];
            *b ^= k;
        }
    }
}

fn keys(secret: &[u8], skey: &[u8]) -> (Rc4, Rc4) {
    let key_a = sha1(&[b"keyA", secret, skey]);
    let key_b = sha1(&[b"keyB", secret, skey]);
    (Rc4::new(&key_a), Rc4::new(&key_b))
}

fn random_pad() -> Vec<u8> {
    let mut rng = rand::rng();
    let len = rng.random_range(0..=MAX_PAD / 4);
    let mut pad = vec![0u8; len];
    rng.fill(&mut pad[..]);
    pad
}

/// Read until `pattern` appears within the first `limit` bytes of the stream.
/// Returns the bytes read *after* the pattern (already received from the peer).
async fn sync_to(
    read: &mut BoxAsyncReadVectored,
    pattern: &[u8],
    limit: usize,
    mut buf: Vec<u8>,
) -> anyhow::Result<Vec<u8>> {
    loop {
        if let Some(pos) = buf.windows(pattern.len()).position(|w| w == pattern) {
            return Ok(buf.split_off(pos + pattern.len()));
        }
        if buf.len() >= limit + pattern.len() {
            bail!("MSE: sync pattern not found");
        }
        let mut tmp = [0u8; 1024];
        let n = read.read(&mut tmp).await?;
        if n == 0 {
            bail!("MSE: peer closed during handshake");
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

/// Reads exactly `n` bytes, first from `pending`, then from the stream.
async fn take_exact(
    read: &mut BoxAsyncReadVectored,
    pending: &mut Vec<u8>,
    n: usize,
) -> anyhow::Result<Vec<u8>> {
    while pending.len() < n {
        let mut tmp = [0u8; 1024];
        let r = read.read(&mut tmp).await?;
        if r == 0 {
            bail!("MSE: peer closed during handshake");
        }
        pending.extend_from_slice(&tmp[..r]);
    }
    let rest = pending.split_off(n);
    Ok(std::mem::replace(pending, rest))
}

/// Outgoing (initiator) handshake. On success returns streams that
/// transparently encrypt/decrypt (or plaintext if the peer selected it).
pub(crate) async fn initiate(
    mut read: BoxAsyncReadVectored,
    mut write: BoxAsyncWrite,
    info_hash: Id20,
    allow_plaintext: bool,
) -> anyhow::Result<(BoxAsyncReadVectored, BoxAsyncWrite)> {
    let dh = Dh::new();
    let mut first = dh.public.to_vec();
    first.extend_from_slice(&random_pad());
    write.write_all(&first).await?;

    let mut yb = [0u8; KEY_LEN];
    read.read_exact(&mut yb).await.context("MSE: reading Yb")?;
    let s = dh.shared_secret(&yb);
    let skey = &info_hash.0;
    let (mut enc_a, dec_b) = keys(&s, skey);

    let req2 = sha1(&[b"req2", skey]);
    let req3 = sha1(&[b"req3", &s]);
    let mut msg = Vec::with_capacity(60);
    msg.extend_from_slice(&sha1(&[b"req1", &s]));
    msg.extend(req2.iter().zip(req3.iter()).map(|(a, b)| a ^ b));
    let provide = if allow_plaintext {
        CRYPTO_RC4 | CRYPTO_PLAINTEXT
    } else {
        CRYPTO_RC4
    };
    let mut tail = Vec::with_capacity(16);
    tail.extend_from_slice(&VC);
    tail.extend_from_slice(&provide.to_be_bytes());
    tail.extend_from_slice(&0u16.to_be_bytes()); // len(PadC)
    tail.extend_from_slice(&0u16.to_be_bytes()); // len(IA): handshake follows normally
    enc_a.apply(&mut tail);
    msg.extend_from_slice(&tail);
    write.write_all(&msg).await?;

    // Find B's encrypted VC after PadB.
    let mut dec_b_probe = dec_b.clone();
    let mut expected_vc = VC;
    dec_b_probe.apply(&mut expected_vc);
    let mut pending = sync_to(&mut read, &expected_vc, MAX_PAD, Vec::new()).await?;
    let mut dec_b = dec_b_probe; // keystream now positioned after VC

    let mut hdr = take_exact(&mut read, &mut pending, 6).await?;
    dec_b.apply(&mut hdr);
    let select = u32::from_be_bytes(hdr[0..4].try_into().unwrap());
    let pad_d = u16::from_be_bytes(hdr[4..6].try_into().unwrap()) as usize;
    if pad_d > MAX_PAD {
        bail!("MSE: padD too long");
    }
    let mut pad = take_exact(&mut read, &mut pending, pad_d).await?;
    dec_b.apply(&mut pad);

    match select {
        CRYPTO_RC4 => Ok((
            Box::new(Rc4Reader::new(read, pending, dec_b)),
            Box::new(Rc4Writer::new(write, enc_a)),
        )),
        CRYPTO_PLAINTEXT if allow_plaintext => {
            Ok((Box::new(PrefixedReader::new(read, pending)), write))
        }
        other => bail!("MSE: peer selected unsupported crypto {other:#x}"),
    }
}

/// Result of inspecting an incoming connection.
pub(crate) enum Accepted {
    /// Plain BitTorrent handshake (already-read bytes are replayed).
    Plain(BoxAsyncReadVectored, BoxAsyncWrite),
    /// Encrypted connection for the torrent with this info hash.
    Encrypted(BoxAsyncReadVectored, BoxAsyncWrite, Id20),
}

/// Incoming (responder) side. Detects plaintext vs MSE from the first bytes.
/// `info_hashes` are the torrents we serve (to identify SKEY).
pub(crate) async fn accept(
    mut read: BoxAsyncReadVectored,
    mut write: BoxAsyncWrite,
    info_hashes: &[Id20],
    policy: EncryptionPolicy,
) -> anyhow::Result<Accepted> {
    let mut start = Vec::with_capacity(KEY_LEN);
    while start.len() < PROTOCOL_HEADER.len() {
        let mut tmp = [0u8; 128];
        let n = read.read(&mut tmp).await?;
        if n == 0 {
            bail!("peer closed before handshake");
        }
        start.extend_from_slice(&tmp[..n]);
    }
    if start.starts_with(PROTOCOL_HEADER) {
        if policy == EncryptionPolicy::Required {
            bail!("plaintext connection refused: encryption required");
        }
        return Ok(Accepted::Plain(
            Box::new(PrefixedReader::new(read, start)),
            write,
        ));
    }
    if policy == EncryptionPolicy::Disabled {
        bail!("encrypted connection refused: encryption disabled");
    }

    let mut pending = start;
    let ya = take_exact(&mut read, &mut pending, KEY_LEN).await?;
    let dh = Dh::new();
    let mut first = dh.public.to_vec();
    first.extend_from_slice(&random_pad());
    write.write_all(&first).await?;
    let s = dh.shared_secret(&ya);

    let req1 = sha1(&[b"req1", &s]);
    let mut pending = sync_to(&mut read, &req1, MAX_PAD, pending).await?;
    let xored = take_exact(&mut read, &mut pending, 20).await?;
    let req3 = sha1(&[b"req3", &s]);
    let req2: Vec<u8> = xored.iter().zip(req3.iter()).map(|(a, b)| a ^ b).collect();
    let info_hash = info_hashes
        .iter()
        .find(|ih| sha1(&[b"req2", &ih.0]).as_slice() == req2.as_slice())
        .copied()
        .context("MSE: no matching torrent")?;

    let (mut dec_a, mut enc_b) = keys(&s, &info_hash.0);
    let mut hdr = take_exact(&mut read, &mut pending, 14).await?;
    dec_a.apply(&mut hdr);
    if hdr[0..8] != VC {
        bail!("MSE: bad verification constant");
    }
    let provide = u32::from_be_bytes(hdr[8..12].try_into().unwrap());
    let pad_c = u16::from_be_bytes(hdr[12..14].try_into().unwrap()) as usize;
    if pad_c > MAX_PAD {
        bail!("MSE: padC too long");
    }
    let mut pad = take_exact(&mut read, &mut pending, pad_c + 2).await?;
    dec_a.apply(&mut pad);
    let ia_len = u16::from_be_bytes(pad[pad_c..pad_c + 2].try_into().unwrap()) as usize;
    let mut ia = take_exact(&mut read, &mut pending, ia_len).await?;
    dec_a.apply(&mut ia);

    let select = if provide & CRYPTO_RC4 != 0 {
        CRYPTO_RC4
    } else if provide & CRYPTO_PLAINTEXT != 0 && policy != EncryptionPolicy::Required {
        CRYPTO_PLAINTEXT
    } else {
        bail!("MSE: no acceptable crypto method offered ({provide:#x})");
    };

    let mut reply = Vec::with_capacity(14);
    reply.extend_from_slice(&VC);
    reply.extend_from_slice(&select.to_be_bytes());
    reply.extend_from_slice(&0u16.to_be_bytes()); // len(padD)
    enc_b.apply(&mut reply);
    write.write_all(&reply).await?;

    // IA (if any) is already-decrypted payload that precedes the stream.
    let (read, write): (BoxAsyncReadVectored, BoxAsyncWrite) = if select == CRYPTO_RC4 {
        (
            Box::new(PrefixedReader::new(
                Box::new(Rc4Reader::new(read, pending, dec_a)),
                ia,
            )),
            Box::new(Rc4Writer::new(write, enc_b)),
        )
    } else {
        let mut prefix = ia;
        prefix.extend_from_slice(&pending);
        (Box::new(PrefixedReader::new(read, prefix)), write)
    };
    Ok(Accepted::Encrypted(read, write, info_hash))
}

// ---------------------------------------------------------------- streams

/// Replays `prefix` before reading from `inner`.
pub(crate) struct PrefixedReader {
    inner: BoxAsyncReadVectored,
    prefix: Vec<u8>,
    pos: usize,
}

impl PrefixedReader {
    pub(crate) fn new(inner: BoxAsyncReadVectored, prefix: Vec<u8>) -> Self {
        Self {
            inner,
            prefix,
            pos: 0,
        }
    }
}

impl AsyncRead for PrefixedReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.pos < this.prefix.len() {
            let n = (this.prefix.len() - this.pos).min(buf.remaining());
            buf.put_slice(&this.prefix[this.pos..this.pos + n]);
            this.pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl AsyncReadVectored for PrefixedReader {
    fn poll_read_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        vec: &mut [IoSliceMut<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.pos < this.prefix.len() {
            let mut total = 0;
            for slice in vec.iter_mut() {
                let n = (this.prefix.len() - this.pos).min(slice.len());
                slice[..n].copy_from_slice(&this.prefix[this.pos..this.pos + n]);
                this.pos += n;
                total += n;
                if this.pos == this.prefix.len() {
                    break;
                }
            }
            return Poll::Ready(Ok(total));
        }
        Pin::new(&mut this.inner).poll_read_vectored(cx, vec)
    }
}

/// Decrypts everything read (including already-received `pending` bytes).
pub(crate) struct Rc4Reader {
    inner: PrefixedReader,
    rc4: Rc4,
}

impl Rc4Reader {
    fn new(inner: BoxAsyncReadVectored, pending: Vec<u8>, rc4: Rc4) -> Self {
        Self {
            inner: PrefixedReader::new(inner, pending),
            rc4,
        }
    }
}

impl AsyncRead for Rc4Reader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        this.rc4.apply(&mut buf.filled_mut()[before..]);
        Poll::Ready(Ok(()))
    }
}

impl AsyncReadVectored for Rc4Reader {
    fn poll_read_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        vec: &mut [IoSliceMut<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        // Read into one buffer we keep our own handle on. Some readers (uTP)
        // advance the caller's IoSliceMut in place, so decrypting `vec` after
        // the call would touch the wrong bytes.
        let Some(slice) = vec.iter_mut().find(|s| !s.is_empty()) else {
            return Poll::Ready(Ok(0));
        };
        let buf: &mut [u8] = slice;
        let n = {
            let mut one = [IoSliceMut::new(&mut *buf)];
            ready!(Pin::new(&mut this.inner).poll_read_vectored(cx, &mut one))?
        };
        this.rc4.apply(&mut buf[..n]);
        Poll::Ready(Ok(n))
    }
}

/// Encrypts everything written. Bytes are encrypted once into `pending`;
/// a partial write reports progress in plaintext units, and the caller
/// (e.g. `write_all`) re-submits the remainder, which is then served from
/// `pending` rather than encrypted twice.
pub(crate) struct Rc4Writer {
    inner: BoxAsyncWrite,
    rc4: Rc4,
    pending: Vec<u8>,
    pos: usize,
}

impl Rc4Writer {
    fn new(inner: BoxAsyncWrite, rc4: Rc4) -> Self {
        Self {
            inner,
            rc4,
            pending: Vec::new(),
            pos: 0,
        }
    }
}

impl AsyncWrite for Rc4Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.pos >= this.pending.len() {
            if buf.is_empty() {
                return Poll::Ready(Ok(0));
            }
            this.pending.clear();
            this.pending.extend_from_slice(buf);
            this.rc4.apply(&mut this.pending);
            this.pos = 0;
        }
        let n = ready!(Pin::new(&mut this.inner).poll_write(cx, &this.pending[this.pos..]))?;
        this.pos += n;
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        while this.pos < this.pending.len() {
            let n = ready!(Pin::new(&mut this.inner).poll_write(cx, &this.pending[this.pos..]))?;
            if n == 0 {
                return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into()));
            }
            this.pos += n;
        }
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_known_vector() {
        // RC4("Key", "Plaintext") = BBF316E8D940AF0AD3 (without the MSE discard).
        let mut s = [0u8; 256];
        for (i, v) in s.iter_mut().enumerate() {
            *v = i as u8;
        }
        let key = b"Key";
        let mut j: u8 = 0;
        for i in 0..256 {
            j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
            s.swap(i, j as usize);
        }
        let mut rc4 = Rc4 { s, i: 0, j: 0 };
        let mut data = *b"Plaintext";
        rc4.apply(&mut data);
        assert_eq!(hex::encode(data), "bbf316e8d940af0ad3");
    }

    #[test]
    fn dh_agrees() {
        let a = Dh::new();
        let b = Dh::new();
        assert_eq!(a.shared_secret(&b.public), b.shared_secret(&a.public));
    }

    #[tokio::test]
    async fn handshake_roundtrip_rc4() {
        let ih = Id20::new([7u8; 20]);
        let (a, b) = tokio::io::duplex(1 << 16);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        use crate::vectored_traits::AsyncReadVectoredIntoCompat;
        let ar: BoxAsyncReadVectored = Box::new(ar.into_vectored_compat());
        let br: BoxAsyncReadVectored = Box::new(br.into_vectored_compat());
        let initiator = tokio::spawn(initiate(ar, Box::new(aw), ih, true));
        let responder = tokio::spawn(async move {
            accept(br, Box::new(bw), &[Id20::new([1u8; 20]), ih], EncryptionPolicy::Enabled).await
        });
        let (mut ar, mut aw) = initiator.await.unwrap().unwrap();
        let (mut br, mut bw, got) = match responder.await.unwrap().unwrap() {
            Accepted::Encrypted(r, w, ih) => (r, w, ih),
            Accepted::Plain(..) => panic!("expected encrypted"),
        };
        assert_eq!(got, ih);
        aw.write_all(b"hello from A").await.unwrap();
        let mut buf = [0u8; 12];
        br.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello from A");
        bw.write_all(b"hi from B").await.unwrap();
        let mut buf = [0u8; 9];
        ar.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hi from B");
    }

    /// A reader that, like uTP, advances the caller's IoSliceMut in place.
    struct AdvancingReader(std::io::Cursor<Vec<u8>>);
    impl AsyncRead for AdvancingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let this = self.get_mut();
            let pos = this.0.position() as usize;
            let data = &this.0.get_ref()[pos..];
            let n = data.len().min(buf.remaining());
            buf.put_slice(&data[..n]);
            this.0.set_position((pos + n) as u64);
            Poll::Ready(Ok(()))
        }
    }
    impl AsyncReadVectored for AdvancingReader {
        fn poll_read_vectored(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            mut vec: &mut [IoSliceMut<'_>],
        ) -> Poll<std::io::Result<usize>> {
            let this = self.get_mut();
            let mut written = 0;
            while let Some(first) = vec.first_mut() {
                if first.is_empty() {
                    vec = &mut vec[1..];
                    continue;
                }
                let pos = this.0.position() as usize;
                let data = &this.0.get_ref()[pos..];
                if data.is_empty() {
                    break;
                }
                let n = data.len().min(first.len());
                first[..n].copy_from_slice(&data[..n]);
                first.advance(n);
                this.0.set_position((pos + n) as u64);
                written += n;
            }
            Poll::Ready(Ok(written))
        }
    }

    #[tokio::test]
    async fn rc4_reader_with_advancing_inner() {
        let key = [3u8; 20];
        let mut plain = b"the quick brown fox jumps over the lazy dog".to_vec();
        let expected = plain.clone();
        Rc4::new(&key).apply(&mut plain); // now ciphertext
        let inner: BoxAsyncReadVectored = Box::new(AdvancingReader(std::io::Cursor::new(plain)));
        let mut r = Rc4Reader::new(inner, Vec::new(), Rc4::new(&key));
        let mut out = vec![0u8; expected.len()];
        let mut got = 0;
        while got < out.len() {
            let take = 5.min(out.len() - got);
            let (a, b) = out[got..].split_at_mut(take);
            let mut v = [IoSliceMut::new(a), IoSliceMut::new(b)];
            let n = std::future::poll_fn(|cx| Pin::new(&mut r).poll_read_vectored(cx, &mut v))
                .await
                .unwrap();
            assert!(n > 0);
            got += n;
        }
        assert_eq!(out, expected);
    }

    #[tokio::test]
    async fn plaintext_detected() {
        let (a, b) = tokio::io::duplex(1 << 16);
        let (_ar, mut aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        use crate::vectored_traits::AsyncReadVectoredIntoCompat;
        let br: BoxAsyncReadVectored = Box::new(br.into_vectored_compat());
        let mut hs = PROTOCOL_HEADER.to_vec();
        hs.extend_from_slice(&[0u8; 48]);
        aw.write_all(&hs).await.unwrap();
        match accept(br, Box::new(bw), &[], EncryptionPolicy::Enabled).await.unwrap() {
            Accepted::Plain(mut r, _) => {
                let mut buf = vec![0u8; hs.len()];
                r.read_exact(&mut buf).await.unwrap();
                assert_eq!(buf, hs);
            }
            Accepted::Encrypted(..) => panic!("expected plain"),
        }
    }
}
