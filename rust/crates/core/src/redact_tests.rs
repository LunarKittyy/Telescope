//! What diagnostics hides beyond tokens and addresses, and what it must leave alone.

use super::*;
use crate::diagnostics::sanitize_at;

fn clean(home: &str, text: &str) -> String {
    sanitize_at(text, Some(Path::new(home)))
}

#[test]
fn the_home_folder_goes_in_every_spelling() {
    let cases: [(&str, &str, &str); 14] = [
        ("/home/luna", "can't read /home/luna/.config/x", "can't read ~/.config/x"),
        ("/home/luna", "/HOME/Luna/x", "~/x"),
        (r"C:\Users\Luna", r"open C:\Users\Luna\AppData\x", r"open ~\AppData\x"),
        (r"C:\Users\Luna", "open C:/Users/Luna/AppData/x", "open ~/AppData/x"),
        (r"C:\Users\Luna", r"open c:\users\LUNA\appdata", r"open ~\appdata"),
        (r"C:\Users\Luna", r"open \\?\C:\Users\Luna\x", r"open ~\x"),
        (r"C:\Users\Luna", r"open C:\\Users\\Luna\\x", r"open ~\\x"),
        (r"C:\Users\Luna", r#"path: "C:\\Users\\Luna\\AppData""#, r#"path: "~\\AppData""#),
        (r"C:\Users\Luna Kitty", r"open C:\Users\Luna Kitty\x", r"open ~\x"),
        (r"C:\Users\Luna", r"open C:\Users\LUNAKI~1\AppData", r"open C:\Users\<user>\AppData"),
        ("/home/luna", "/home/emma/.cache", "/home/<user>/.cache"),
        ("/home/luna", "/Users/emma/Library/x", "/Users/<user>/Library/x"),
        ("/home/luna", r"D:\Users\Zoe\x and /var/home/kit/y", r"D:\Users\<user>\x and /var/home/<user>/y"),
        ("/home/luna", "file:///home/luna/a.png", "file:~/a.png"),
    ];
    for (home, text, want) in cases {
        assert_eq!(clean(home, text), want, "{text}");
    }
}

#[test]
fn shared_user_folders_and_bare_words_stay() {
    for text in [r"C:\Users\Public\Documents", r"C:\Users\Default\x", "/Users/Shared/x", "/home/ is a folder", "users and home"] {
        assert_eq!(clean("/home/luna", text), text);
    }
}

#[test]
fn the_account_name_goes_from_the_log_folder() {
    let who = Who::new(Some(Path::new("/home/luna")), Some("luna"), None);
    assert_eq!(hide_homes("log /tmp/telescope-luna/telescope.log and /tmp/Telescope-LUNA/x", &who), "log /tmp/telescope-<user>/telescope.log and /tmp/Telescope-<user>/x");
    assert_eq!(hide_homes("telescope-lunar stays", &who), "telescope-lunar stays");
}

#[test]
fn addresses_ids_and_names_go() {
    let cases: [(&str, &str); 22] = [
        ("adapter aa:bb:cc:dd:ee:ff up", "adapter <mac> up"),
        ("mac AA-BB-CC-DD-EE-FF.", "mac <mac>."),
        ("contact luna@example.com now", "contact <email> now"),
        ("from a.b+c@mail.example.co.uk,", "from <email>,"),
        ("id 123e4567-e89b-12d3-a456-426614174000 ok", "id <uuid> ok"),
        ("id {123E4567-E89B-12D3-A456-426614174000}", "id {<uuid>}"),
        ("serial 0A1B2C3D4E5F6A7B end", "serial <id> end"),
        ("serial=abcdef123456", "serial=<id>"),
        ("sha 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08", "sha <id>"),
        ("found pixel-7.local at once", "found <host> at once"),
        ("nas.home.lan down", "<host> down"),
        ("printer.localdomain", "<host>"),
        ("Computer: DESKTOP-AB12CD3", "Computer: <name>"),
        ("COMPUTERNAME=LUNA-PC", "COMPUTERNAME=<name>"),
        ("Computer name: Luna's PC, 8 cores", "Computer name: <name>, 8 cores"),
        ("hostname: kitty-laptop", "hostname: <name>"),
        ("USERNAME=Luna", "USERNAME=<name>"),
        ("on DESKTOP-AB12CD3 and LAPTOP-9XYZ8W7Q", "on <computer> and <computer>"),
        ("on WIN-1A2B3C4D5E6", "on <computer>"),
        ("phone 10.0.0.5 mac 11:22:33:44:55:66", "phone <ip> mac <mac>"),
        ("Bearer abcdef123456abcdef123456", "Bearer <redacted>"),
        ("cert 5f:ab:12:cd:34:ef", "cert <mac>"),
    ];
    for (text, want) in cases {
        assert_eq!(clean("/home/luna", text), want, "{text}");
    }
}

#[test]
fn numbers_versions_and_names_of_things_stay() {
    let kept = [
        "Telescope 5.0.1 (38f8aff7)",
        "Streaming 1920 \u{d7} 1080 at 29.97 fps, 12.4 Mbps, 30 fps to OBS Virtual Camera",
        "Decoding on vaapi: p95 12.3 ms, max 2048.5 ms a picture over the last 3 s",
        "codec h264 profile high level 4.1, hevc, av1, mjpeg, vp9",
        "OS: Windows 11 (10.0.26200)",
        "OS: Nobara Linux 44 (7.2.11-200.nobara.fc44.x86_64)",
        "GPU: NVIDIA GeForce RTX 3060, dedicated, driver 560.35.03",
        "Mesa 24.1.5 on AMD Radeon 780M Graphics (radeonsi)",
        "time 1730000000000 ms and frame 123456789012345",
        "asset icon@2x.png loaded, icon@3x.webp too",
        "settings.local.json saved, Cargo.local.toml",
        "pointer 0x7ffd12345678 ok",
        "12:30:45 stream dropped",
        "ffmpeg 7.1.1, wgpu 29.0, Slint 1.15",
        "Unreachable: no route, user cancelled, Computer was asleep",
        "decoder: hardware (d3d11va), 4 cores, 16 GB, on battery: no",
    ];
    for text in kept {
        assert_eq!(clean("/home/luna", text), text);
    }
}

#[test]
fn the_computers_own_name_goes_unless_it_is_a_plain_word() {
    let who = Who::new(None, None, Some("Lunas-PC.local"));
    assert_eq!(hide_host("started on lunas-pc, then LUNAS-PC again", &who), "started on <host>, then <host> again");
    assert_eq!(hide_host("lunas-pcs stays", &who), "lunas-pcs stays");
    for plain in ["localhost", "fedora", "abc"] {
        let who = Who::new(None, None, Some(plain));
        assert_eq!(hide_host("on fedora and localhost", &who), "on fedora and localhost");
    }
}

#[test]
fn model_strings_are_told_from_names_people_chose() {
    for model in ["Pixel 7 Pro", "Logitech HD Pro Webcam C920", "Galaxy S23 Ultra", "Integrated Camera: Integrated C", "OBS Virtual Camera", "USB2.0 HD UVC WebCam", "v4l2loopback"] {
        assert!(is_model_string(model), "{model}");
    }
    for chosen in ["Luna's Pixel", "Pixel 7 Luna", "Emma iPhone", "My Galaxy", "Galaxy of Luna", "Kitchen cam", ""] {
        assert!(!is_model_string(chosen), "{chosen}");
    }
}

#[test]
fn chosen_names_become_a_neutral_label_and_models_stay() {
    let hidden = Hidden::of("phone", &["Luna's Pixel", "Emma iPhone", "Pixel 7 Pro", "phone", "ab"]);
    assert_eq!(hidden.apply("from Luna's Pixel and emma iphone, Pixel 7 Pro, phone, ab"), "from phone 1 and phone 2, Pixel 7 Pro, phone, ab");
    assert_eq!(hidden.apply("EMMA IPHONE's battery"), "phone 2's battery");
    assert_eq!(hidden.apply("xEmma iPhonex stays"), "xEmma iPhonex stays");
}

#[test]
fn the_names_plugins_hand_over_are_hidden_from_the_log() {
    set_hidden("computer", &["Luna's Desktop".to_string()]);
    assert_eq!(crate::diagnostics::sanitize("Waiting for Luna's Desktop to answer"), "Waiting for computer 1 to answer");
    set_hidden("computer", &[]);
    assert_eq!(crate::diagnostics::sanitize("Waiting for Luna's Desktop to answer"), "Waiting for Luna's Desktop to answer");
}
