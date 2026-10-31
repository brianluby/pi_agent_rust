//! Bounded executable discovery using the runtime's configured environment.
//! This examines file metadata; it never executes a shell or a candidate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rquickjs::function::{Func, Opt};
use rquickjs::{Ctx, Exception};

use super::{ExecMediationResult, ExtensionPolicy, check_exec_capability, current_extension_id};

const MAX_PATH_BYTES: usize = 32 * 1024;
const MAX_PATH_ENTRIES: usize = 256;
const MAX_MATCHES: usize = 16;

fn simple_program_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

fn configured_env<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(String::as_str).or_else(|| {
        // Windows environment keys are case-insensitive. Prefer the exact
        // key above; sort aliases so even a synthetic duplicate is stable.
        if cfg!(windows) {
            env.iter()
                .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
                .min_by(|(left, _), (right, _)| left.cmp(right))
                .map(|(_, value)| value.as_str())
        } else {
            None
        }
    })
}

fn program_candidates(name: &str, extensions: Option<&str>, windows: bool) -> Vec<String> {
    if !windows || Path::new(name).extension().is_some() {
        return vec![name.to_string()];
    }
    extensions
        .unwrap_or(".COM;.EXE;.BAT;.CMD")
        .split(';')
        .take(16)
        .filter(|extension| {
            extension.starts_with('.')
                && (2..=16).contains(&extension.len())
                && extension.as_bytes()[1..]
                    .iter()
                    .all(u8::is_ascii_alphanumeric)
        })
        .map(|extension| format!("{name}{extension}"))
        .collect()
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn find_executables(
    name: &str,
    env: &HashMap<String, String>,
    cwd: &Path,
) -> std::result::Result<Vec<String>, String> {
    if !simple_program_name(name) {
        return Err("executable discovery requires a simple program name".to_string());
    }
    let path = configured_env(env, "PATH").unwrap_or("");
    if path.len() > MAX_PATH_BYTES {
        return Err("configured executable search path exceeds 32 KiB".to_string());
    }
    // An absent PATH never falls back to the host's ambient environment.
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let names = program_candidates(name, configured_env(env, "PATHEXT"), cfg!(windows));
    let mut found = Vec::new();
    let paths: Vec<_> = std::env::split_paths(path)
        .take(MAX_PATH_ENTRIES + 1)
        .collect();
    if paths.len() > MAX_PATH_ENTRIES {
        return Err("configured executable search path exceeds 256 entries".to_string());
    }
    for directory in paths {
        let directory = if directory.is_absolute() {
            directory
        } else {
            cwd.join(directory)
        };
        for name in &names {
            let candidate = directory.join(name);
            if !executable_file(&candidate) {
                continue;
            }
            let Ok(candidate) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            let candidate = candidate.to_string_lossy().into_owned();
            if !found.contains(&candidate) {
                found.push(candidate);
            }
            if found.len() == MAX_MATCHES {
                return Ok(found);
            }
        }
    }
    Ok(found)
}

pub(super) fn install(
    ctx: &Ctx<'_>,
    env: HashMap<String, String>,
    process_cwd: String,
    policy: Option<ExtensionPolicy>,
) -> rquickjs::Result<()> {
    ctx.globals().set(
        "__pi_find_executables_native",
        Func::from(move |ctx: Ctx<'_>, name: String, cwd: Opt<String>| {
            let extension_id = current_extension_id(&ctx);
            if let Some(policy) = &policy {
                if !check_exec_capability(policy, extension_id.as_deref()) {
                    return Err(Exception::throw_message(
                        &ctx,
                        "executable discovery requires exec capability",
                    ));
                }
                if matches!(
                    super::evaluate_exec_mediation(&policy.exec_mediation, &name, &[]),
                    ExecMediationResult::Deny { .. }
                ) {
                    return Err(Exception::throw_message(
                        &ctx,
                        "executable discovery denied by exec mediation",
                    ));
                }
            }
            let base = Path::new(&process_cwd);
            let requested = cwd.0.filter(|value| !value.is_empty()).map(PathBuf::from);
            let cwd = requested.map_or_else(
                || base.to_path_buf(),
                |path| {
                    if path.is_absolute() {
                        path
                    } else {
                        base.join(path)
                    }
                },
            );
            let found = find_executables(&name, &env, &cwd)
                .map_err(|message| Exception::throw_message(&ctx, &message))?;
            serde_json::to_string(&found)
                .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_never_interprets_paths_or_shell_syntax() {
        for name in [
            "../node",
            "C:node",
            "node.exe:stream",
            "node;id",
            "$(id)",
            "-v",
            "node\0",
            "node node",
        ] {
            assert!(
                find_executables(name, &HashMap::new(), Path::new(".")).is_err(),
                "{name:?}"
            );
        }
        assert!(
            find_executables("node", &HashMap::new(), Path::new("."))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn windows_extensions_are_bounded_and_never_paths() {
        assert_eq!(
            program_candidates("node", Some(".EXE;.CMD"), true),
            ["node.EXE", "node.CMD"]
        );
        assert_eq!(
            program_candidates("node.exe", Some(".CMD"), true),
            ["node.exe"]
        );
        assert_eq!(
            program_candidates("node", Some(".exe/../../secret;.BAT"), true),
            ["node.BAT"]
        );
        assert_eq!(program_candidates("node", Some(".EXE"), false), ["node"]);
    }

    #[test]
    fn discovery_uses_configured_path_in_order_without_running_candidates() {
        let root = tempfile::tempdir().expect("tempdir");
        let first = root.path().join("path with spaces");
        let second = root.path().join("second");
        std::fs::create_dir(&first).expect("first path");
        std::fs::create_dir(&second).expect("second path");
        let filename = if cfg!(windows) {
            "fixture.EXE"
        } else {
            "fixture"
        };
        for directory in [&first, &second] {
            let file = directory.join(filename);
            std::fs::write(&file, "This is a metadata fixture, never executable code.")
                .expect("fixture");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700))
                    .expect("mode");
            }
        }
        let env = HashMap::from([
            (
                "PATH".to_string(),
                std::env::join_paths([&first, &second, &first])
                    .expect("path")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("PATHEXT".to_string(), ".EXE".to_string()),
        ]);
        let found = find_executables("fixture", &env, root.path()).expect("discover");
        let expected: Vec<_> = [&first, &second]
            .iter()
            .map(|directory| {
                std::fs::canonicalize(directory.join(filename))
                    .expect("canonical")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(found, expected);
    }
}
