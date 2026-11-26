//! "Open magnet links and .torrent files with Rustorrent".

/// Register Rustorrent as the handler; returns a message for the user.
pub fn register_handlers() -> anyhow::Result<String> {
    imp()
}

#[cfg(target_os = "windows")]
fn imp() -> anyhow::Result<String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let exe = std::env::current_exe()?.display().to_string();
    let open_cmd = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    let (magnet, _) = hkcu.create_subkey(r"Software\Classes\magnet")?;
    magnet.set_value("", &"URL:Magnet link")?;
    magnet.set_value("URL Protocol", &"")?;
    magnet
        .create_subkey("DefaultIcon")?
        .0
        .set_value("", &icon)?;
    magnet
        .create_subkey(r"shell\open\command")?
        .0
        .set_value("", &open_cmd)?;

    let (prog, _) = hkcu.create_subkey(r"Software\Classes\Rustorrent.Torrent")?;
    prog.set_value("", &"BitTorrent file")?;
    prog.create_subkey("DefaultIcon")?.0.set_value("", &icon)?;
    prog.create_subkey(r"shell\open\command")?
        .0
        .set_value("", &open_cmd)?;

    let (ext, _) = hkcu.create_subkey(r"Software\Classes\.torrent")?;
    ext.set_value("", &"Rustorrent.Torrent")?;
    ext.set_value("Content Type", &"application/x-bittorrent")?;

    Ok("Magnet links and .torrent files now open in Rustorrent. \
        Windows may ask you to confirm the first time."
        .into())
}

#[cfg(target_os = "macos")]
fn imp() -> anyhow::Result<String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSSetDefaultHandlerForURLScheme(scheme: CFStringRef, bundle_id: CFStringRef) -> i32;
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
            bundle_id: CFStringRef,
        ) -> i32;
    }
    const ROLES_ALL: u32 = 0xFFFF_FFFF;
    let bundle = CFString::new(crate::platform::register::MACOS_BUNDLE_ID);
    let magnet = CFString::new("magnet");
    let torrent = CFString::new("org.bittorrent.torrent");
    let (a, b) = unsafe {
        (
            LSSetDefaultHandlerForURLScheme(
                magnet.as_concrete_TypeRef(),
                bundle.as_concrete_TypeRef(),
            ),
            LSSetDefaultRoleHandlerForContentType(
                torrent.as_concrete_TypeRef(),
                ROLES_ALL,
                bundle.as_concrete_TypeRef(),
            ),
        )
    };
    if a != 0 || b != 0 {
        anyhow::bail!(
            "macOS refused (codes {a}, {b}). Make sure you're running Rustorrent.app \
             (moved to Applications), then try again."
        );
    }
    Ok("Magnet links and .torrent files now open in Rustorrent.".into())
}

#[cfg(target_os = "macos")]
pub const MACOS_BUNDLE_ID: &str = "io.github.rustorrent";

#[cfg(all(unix, not(target_os = "macos")))]
fn imp() -> anyhow::Result<String> {
    use anyhow::Context;
    let data = dirs::data_dir().context("no data directory")?;
    let apps = data.join("applications");
    let icons = data.join("icons/hicolor/256x256/apps");
    std::fs::create_dir_all(&apps)?;
    std::fs::create_dir_all(&icons)?;
    std::fs::write(
        icons.join("rustorrent.png"),
        include_bytes!("../../assets/icon-256.png"),
    )?;
    let exe = std::env::current_exe()?.display().to_string();
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName=Rustorrent\nComment=Fast, simple BitTorrent client\n\
         Exec=\"{}\" %U\nIcon=rustorrent\nTerminal=false\nCategories=Network;FileTransfer;P2P;\n\
         MimeType=application/x-bittorrent;x-scheme-handler/magnet;\n",
        exe.replace('"', "\\\"")
    );
    std::fs::write(apps.join("rustorrent.desktop"), desktop)?;
    let run = |args: &[&str]| {
        std::process::Command::new(args[0])
            .args(&args[1..])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    let ok_magnet = run(&[
        "xdg-mime",
        "default",
        "rustorrent.desktop",
        "x-scheme-handler/magnet",
    ]);
    let ok_torrent = run(&[
        "xdg-mime",
        "default",
        "rustorrent.desktop",
        "application/x-bittorrent",
    ]);
    let apps_dir = apps.display().to_string();
    let _ = run(&["update-desktop-database", &apps_dir]);
    if !(ok_magnet && ok_torrent) {
        anyhow::bail!(
            "Installed the Rustorrent menu entry, but couldn't set it as the default \
             (xdg-mime is missing). You can choose Rustorrent in your desktop's \
             \"Default applications\" settings."
        );
    }
    Ok("Magnet links and .torrent files now open in Rustorrent. Rustorrent was also added to your applications menu.".into())
}
