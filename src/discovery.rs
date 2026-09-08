use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
};

use anyhow::{bail, Result};

const HIROSHIMA_MAIL_DOMAIN: &str = "@hiroshima-u.ac.jp";
const OUTLOOK_IMAP_HOST: &str = "outlook.office365.com";

pub fn default_data_dir() -> PathBuf {
    if let Some(path) = env::var_os("HIRO_MAILD_DATA_DIR") {
        return PathBuf::from(path);
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("hiro-maild");
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(local) = env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("hiro-maild");
        }
    }

    if let Some(xdg) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("hiro-maild");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("hiro-maild");
    }

    PathBuf::from("data")
}

fn thunderbird_profile_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".thunderbird"));
        roots.push(home.join("Library").join("Thunderbird").join("Profiles"));
    }

    if let Some(appdata) = env::var_os("APPDATA") {
        roots.push(PathBuf::from(appdata).join("Thunderbird").join("Profiles"));
    }

    roots
}

pub fn find_thunderbird_profiles() -> Vec<PathBuf> {
    let mut profiles = Vec::new();
    for root in thunderbird_profile_roots() {
        if !root.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("prefs.js").is_file() {
                profiles.push(path);
            }
        }
    }
    profiles.sort();
    profiles.dedup();
    profiles
}

pub fn find_hiroshima_stores() -> Vec<PathBuf> {
    let mut stores = Vec::new();

    for profile in find_thunderbird_profiles() {
        let prefs = match read_string_prefs(&profile.join("prefs.js")) {
            Ok(value) => value,
            Err(_) => continue,
        };

        for server_id in hiroshima_server_ids(&prefs) {
            let hostname = prefs
                .get(&format!("mail.server.{server_id}.hostname"))
                .map(String::as_str)
                .unwrap_or_default();
            if !hostname.eq_ignore_ascii_case(OUTLOOK_IMAP_HOST) {
                continue;
            }

            let path = prefs
                .get(&format!("mail.server.{server_id}.directory-rel"))
                .and_then(|relative| resolve_profile_relative(&profile, relative))
                .or_else(|| {
                    find_matching_outlook_store(&profile.join("ImapMail"), server_id, &prefs)
                });

            if let Some(path) = path.filter(|path| store_looks_like_mailbox(path)) {
                stores.push(path);
            }
        }
    }

    stores.sort();
    stores.dedup();
    stores
}

fn hiroshima_server_ids(prefs: &HashMap<String, String>) -> Vec<String> {
    let mut ids = prefs
        .iter()
        .filter_map(|(key, value)| {
            let id = key
                .strip_prefix("mail.server.")?
                .strip_suffix(".userName")?;
            if value.to_ascii_lowercase().ends_with(HIROSHIMA_MAIL_DOMAIN) {
                Some(id.to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn read_string_prefs(path: &Path) -> Result<HashMap<String, String>> {
    let content = std::fs::read_to_string(path)?;
    let mut prefs = HashMap::new();

    for line in content.lines() {
        let Some(rest) = line.trim().strip_prefix("user_pref(\"") else {
            continue;
        };
        let Some((key, value_part)) = rest.split_once("\", ") else {
            continue;
        };
        let Some(value_part) = value_part.strip_prefix('"') else {
            continue;
        };
        let Some(value) = value_part.strip_suffix("\");") else {
            continue;
        };
        prefs.insert(key.to_string(), unescape_pref_string(value));
    }

    Ok(prefs)
}

fn unescape_pref_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn resolve_profile_relative(profile: &Path, relative: &str) -> Option<PathBuf> {
    let relative = relative.strip_prefix("[ProfD]")?;
    let mut path = profile.to_path_buf();
    for component in relative.split(|ch| ch == '/' || ch == '\\') {
        if !component.is_empty() && component != "." {
            path.push(component);
        }
    }
    Some(path)
}

fn find_matching_outlook_store(
    imap_root: &Path,
    server_id: &str,
    prefs: &HashMap<String, String>,
) -> Option<PathBuf> {
    let configured_dir = prefs
        .get(&format!("mail.server.{server_id}.directory"))
        .map(PathBuf::from)
        .filter(|path| path.is_dir());
    if configured_dir.is_some() {
        return configured_dir;
    }

    let entries = std::fs::read_dir(imap_root).ok()?;
    let mut candidates = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .starts_with(OUTLOOK_IMAP_HOST)
        })
        .collect::<Vec<_>>();
    candidates.sort();

    if candidates.len() == 1 {
        candidates.pop()
    } else {
        None
    }
}

fn store_looks_like_mailbox(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let p = entry.path();
        p.is_file()
            && (p.extension().and_then(|v| v.to_str()) == Some("msf")
                || p.extension().is_none())
    })
}

pub fn select_hiroshima_store() -> Result<PathBuf> {
    if let Some(path) = env::var_os("HIRO_MAILD_THUNDERBIRD_STORE") {
        let path = PathBuf::from(path);
        if path.is_dir() {
            return Ok(path);
        }
        bail!(
            "HIRO_MAILD_THUNDERBIRD_STORE does not point to a directory: {}",
            path.display()
        );
    }

    let stores = find_hiroshima_stores();
    match stores.as_slice() {
        [only] => Ok(only.clone()),
        [] => bail!(
            "no Hiroshima University Thunderbird IMAP store found. Configure the @hiroshima-u.ac.jp account in Thunderbird, let it synchronize, then run `hiro-maild doctor`"
        ),
        _ => {
            let list = stores
                .iter()
                .map(|p| format!("  - {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n");
            bail!("multiple Hiroshima University IMAP stores found; rerun with --store PATH:\n{list}")
        }
    }
}

pub fn run_doctor(data_dir: &Path) -> Result<()> {
    println!("data_dir: {}", data_dir.display());
    println!("data_dir_writable: {}", writable(data_dir));

    let profiles = find_thunderbird_profiles();
    println!("thunderbird_profiles: {}", profiles.len());
    for profile in &profiles {
        println!("  profile: {}", profile.display());
    }

    let stores = find_hiroshima_stores();
    println!("hiroshima_imap_stores: {}", stores.len());
    for store in &stores {
        println!("  store: {}", store.display());
    }

    println!(
        "openai_api_key: {}",
        if env::var_os("OPENAI_API_KEY").is_some() {
            "present"
        } else {
            "missing (only required for `triage`)"
        }
    );

    if stores.is_empty() {
        println!("next_action: configure Hiroshima University mail in Thunderbird with IMAP/OAuth2 and enable local synchronization");
    } else if stores.len() > 1 {
        println!("next_action: choose the Hiroshima University store and pass it with --store");
    } else {
        println!("next_action: none for ingestion; `hiro-maild sync` can run");
    }

    Ok(())
}

fn writable(path: &Path) -> bool {
    let probe = path.join(".hiro-maild-write-test");
    match std::fs::write(&probe, b"ok") {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_string_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = dir.path().join("prefs.js");
        std::fs::write(
            &prefs,
            r#"user_pref("mail.server.server1.hostname", "outlook.office365.com");
user_pref("mail.server.server1.userName", "example@hiroshima-u.ac.jp");
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/outlook.office365.com");
"#,
        )
        .unwrap();

        let parsed = read_string_prefs(&prefs).unwrap();
        assert_eq!(
            parsed.get("mail.server.server1.userName").map(String::as_str),
            Some("example@hiroshima-u.ac.jp")
        );
        assert_eq!(hiroshima_server_ids(&parsed), vec!["server1"]);
    }

    #[test]
    fn resolves_profile_relative_directory() {
        let profile = Path::new("/tmp/profile");
        assert_eq!(
            resolve_profile_relative(profile, "[ProfD]ImapMail/outlook.office365.com"),
            Some(profile.join("ImapMail").join("outlook.office365.com"))
        );
    }
}
