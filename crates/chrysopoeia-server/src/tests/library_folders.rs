//! Folders that can't be a library: the whole server, the folder holding
//! the database, the app's own folder and the system folders. The API
//! refuses them and the folder picker is told why.

use axum::http::StatusCode;
use serde_json::json;

use super::support::*;
use crate::services::library_admin::{OwnFolders, library_folder_refusal};

fn enc(p: &std::path::Path) -> String {
    p.to_str()
        .unwrap()
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('#', "%23")
}

/// A server whose browse root is the temp root, so the data folder (and the
/// folder above it) can be browsed.
async fn app_browsing_everything() -> TestApp {
    TestApp::start(TestOptions {
        configure: Box::new(|c, root| c.browse_roots = vec![root.to_path_buf()]),
        ..TestOptions::default()
    })
    .await
}

async fn create(app: &TestApp, path: &str) -> Resp {
    app.post("/api/libraries", json!({ "path": path })).await
}

fn assert_refused(r: &Resp, what: &str) {
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{what}: {}", r.text);
    assert_eq!(r.json["code"], "folder_not_allowed", "{what}: {}", r.text);
    assert_eq!(r.json["field"], "path", "{what}: {}", r.text);
    let error = r.json["error"].as_str().unwrap();
    assert!(error.ends_with('.'), "{what}: {error}");
    assert!(!error.contains("folder_not_allowed"), "{what}: {error}");
}

fn own(root: &std::path::Path) -> OwnFolders {
    let data = root.join("data");
    let web = root.join("web");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&web).unwrap();
    OwnFolders::resolve(&data, &web)
}

#[test]
fn the_whole_server_is_never_a_library() {
    let dir = tempfile::tempdir().unwrap();
    let reason = library_folder_refusal(std::path::Path::new("/"), &own(dir.path())).unwrap();
    assert!(
        reason.starts_with("The whole server can't be a library"),
        "{reason}"
    );
    assert!(
        reason.contains("Choose the folder that holds your videos."),
        "{reason}"
    );
}

#[test]
fn the_settings_folder_and_what_is_inside_or_above_it_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let own = own(&root);
    let data = root.join("data");

    let at = library_folder_refusal(&data, &own).unwrap();
    assert!(
        at.contains(&format!(
            "{} is where Chrysopoeia keeps its database",
            data.display()
        )),
        "{at}"
    );
    let inside = library_folder_refusal(&data.join("backups/2026"), &own).unwrap();
    assert!(
        inside.contains("keeps its database and settings"),
        "{inside}"
    );

    // The folder above holds the data folder; so does each one higher up.
    for above in [root.clone(), root.parent().unwrap().to_path_buf()] {
        let reason = library_folder_refusal(&above, &own).unwrap();
        assert!(
            reason.contains("contains Chrysopoeia's own settings folder"),
            "{}: {reason}",
            above.display()
        );
        assert!(reason.contains(&data.display().to_string()), "{reason}");
    }

    // Next to it is fine, and a folder whose name only starts the same.
    assert_eq!(library_folder_refusal(&root.join("media"), &own), None);
    assert_eq!(library_folder_refusal(&root.join("data2"), &own), None);
    assert_eq!(library_folder_refusal(&root.join("database"), &own), None);
}

#[test]
fn the_apps_own_folder_and_the_system_folders_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let own = own(&root);

    let web = root.join("web");
    let reason = library_folder_refusal(&web, &own).unwrap();
    assert!(
        reason.contains("holds the Chrysopoeia app itself"),
        "{reason}"
    );
    assert!(reason.contains("read-only"), "{reason}");
    assert!(library_folder_refusal(&web.join("_next/static"), &own).is_some());
    assert_eq!(library_folder_refusal(&root.join("webcam"), &own), None);

    // The image's own folders, whatever DATA_DIR and WEB_DIR say.
    for (path, words) in [
        ("/config", "keeps its database and settings"),
        ("/config/backups", "keeps its database and settings"),
        ("/app", "holds the Chrysopoeia app itself"),
        ("/app/web", "holds the Chrysopoeia app itself"),
        ("/proc", "is a system folder"),
        ("/proc/1/root", "is a system folder"),
        ("/sys", "is a system folder"),
        ("/sys/kernel", "is a system folder"),
        ("/dev", "is a system folder"),
        ("/dev/shm", "is a system folder"),
    ] {
        let reason = library_folder_refusal(std::path::Path::new(path), &own)
            .unwrap_or_else(|| panic!("{path} was allowed"));
        assert!(reason.contains(words), "{path}: {reason}");
    }
    // Folders that merely look like them are ordinary.
    for path in [
        "/media",
        "/media/proc",
        "/mnt/user/media",
        "/configs",
        "/apple",
        "/devices",
        "/srv/sys",
    ] {
        assert_eq!(
            library_folder_refusal(std::path::Path::new(path), &own),
            None,
            "{path}"
        );
    }
}

#[tokio::test]
async fn the_api_refuses_a_library_made_of_the_whole_server() {
    let app = TestApp::new().await;
    let r = create(&app, "/").await;
    assert_refused(&r, "/");
    assert!(
        r.json["error"]
            .as_str()
            .unwrap()
            .starts_with("The whole server can't be a library"),
        "{}",
        r.text
    );
    // Written another way, it is still the root.
    for path in ["//", "/.", "/proc/.."] {
        let r = create(&app, path).await;
        assert_refused(&r, path);
    }
    let libs = app.get("/api/libraries").await;
    assert_eq!(libs.json.as_array().unwrap().len(), 0, "{}", libs.text);
}

#[tokio::test]
async fn the_api_refuses_the_settings_folder_and_the_folder_above_it() {
    let app = TestApp::new().await;
    let data = app.dir.path().join("data");
    std::fs::create_dir_all(data.join("backups")).unwrap();

    let at = create(&app, data.to_str().unwrap()).await;
    assert_refused(&at, "the data folder");
    assert!(
        at.json["error"]
            .as_str()
            .unwrap()
            .contains("keeps its database and settings"),
        "{}",
        at.text
    );
    let inside = create(&app, data.join("backups").to_str().unwrap()).await;
    assert_refused(&inside, "inside the data folder");

    // The temp root holds both the data folder and the media folder.
    let above = create(&app, app.dir.path().to_str().unwrap()).await;
    assert_refused(&above, "above the data folder");
    assert!(
        above.json["error"]
            .as_str()
            .unwrap()
            .contains("contains Chrysopoeia's own settings folder"),
        "{}",
        above.text
    );

    // A link to the data folder is the data folder.
    #[cfg(unix)]
    {
        let shortcut = app.media.join("shortcut");
        std::os::unix::fs::symlink(&data, &shortcut).unwrap();
        let r = create(&app, shortcut.to_str().unwrap()).await;
        assert_refused(&r, "a link to the data folder");
    }

    // Nothing was created, and an ordinary folder next to it still works.
    let libs = app.get("/api/libraries").await;
    assert_eq!(libs.json.as_array().unwrap().len(), 0, "{}", libs.text);
    std::fs::create_dir_all(app.media.join("Movies")).unwrap();
    let ok = create(&app, app.media.join("Movies").to_str().unwrap()).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.text);
}

#[tokio::test]
async fn the_api_refuses_the_apps_folder_and_the_system_folders() {
    let app = TestApp::new().await;
    let web = app.dir.path().join("web");
    std::fs::create_dir_all(web.join("_next")).unwrap();
    let r = create(&app, web.to_str().unwrap()).await;
    assert_refused(&r, "the web folder");
    assert!(
        r.json["error"].as_str().unwrap().contains("read-only"),
        "{}",
        r.text
    );
    let r = create(&app, web.join("_next").to_str().unwrap()).await;
    assert_refused(&r, "inside the web folder");

    for path in ["/proc", "/sys", "/dev"] {
        if std::path::Path::new(path).is_dir() {
            let r = create(&app, path).await;
            assert_refused(&r, path);
            assert!(
                r.json["error"].as_str().unwrap().contains("system folder"),
                "{path}: {}",
                r.text
            );
        }
    }
}

#[tokio::test]
async fn libraries_from_the_start_up_list_follow_the_same_rule() {
    let app = TestApp::start(TestOptions {
        configure: Box::new(|c, root| {
            std::fs::create_dir_all(root.join("media/Movies")).unwrap();
            c.libraries = vec![
                std::path::PathBuf::from("/"),
                root.join("data"),
                root.join("media/Movies"),
            ];
        }),
        ..TestOptions::default()
    })
    .await;
    let libs = app.get("/api/libraries").await;
    let names: Vec<&str> = libs
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Movies"], "{}", libs.text);
}

#[tokio::test]
async fn the_folder_picker_is_told_which_folders_can_not_be_a_library() {
    let app = app_browsing_everything().await;
    std::fs::create_dir_all(app.dir.path().join("data/backups")).unwrap();
    let root = app.dir.path().canonicalize().unwrap();

    // The folder above the data folder holds it.
    let r = app.get("/api/fs/browse").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["path"], root.to_str().unwrap());
    assert!(
        r.json["library_blocked"]
            .as_str()
            .unwrap()
            .contains("contains Chrysopoeia's own settings folder"),
        "{}",
        r.text
    );

    // The data folder and the folders inside it.
    for folder in [root.join("data"), root.join("data/backups")] {
        let r = app
            .get(&format!("/api/fs/browse?path={}", enc(&folder)))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert!(
            r.json["library_blocked"]
                .as_str()
                .unwrap()
                .contains("keeps its database and settings"),
            "{}: {}",
            folder.display(),
            r.text
        );
    }

    // The media folder is free: the field is left out.
    let r = app
        .get(&format!("/api/fs/browse?path={}", enc(&root.join("media"))))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json.get("library_blocked").is_none(), "{}", r.text);
}

#[tokio::test]
async fn the_picker_can_still_browse_the_whole_server_it_may_show() {
    // Output and work folders can be anywhere, so browsing isn't limited:
    // only the answer says the folder can't be a library.
    let app = TestApp::start(TestOptions {
        configure: Box::new(|c, _| c.browse_roots = vec![std::path::PathBuf::from("/")]),
        ..TestOptions::default()
    })
    .await;
    let r = app.get("/api/fs/browse?path=/").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["path"], "/");
    assert!(
        r.json["library_blocked"]
            .as_str()
            .unwrap()
            .starts_with("The whole server can't be a library"),
        "{}",
        r.text
    );
}
