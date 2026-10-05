//! `/api/fs/browse` for the folder picker: the folders mounted into the
//! container ("your folders") are listed on their own, and the container's
//! own system folders are marked.
//!
//! `fs_guard::hang::mount` stands in for mounting a folder (dropping its
//! guard unmounts it); the lines of a real container's mount list are in
//! `services::user_folders`'s own tests.

use axum::http::StatusCode;

use super::support::*;
use crate::services::fs_guard::hang;

fn enc(p: &std::path::Path) -> String {
    p.to_str()
        .unwrap()
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('#', "%23")
}

fn user_folder_paths(json: &serde_json::Value) -> Vec<String> {
    json["user_folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect()
}

/// A server whose browse root is the temp root, so the folders mounted
/// there can all be shown.
async fn app_browsing_the_temp_root() -> TestApp {
    TestApp::start(TestOptions {
        configure: Box::new(|c, root| c.browse_roots = vec![root.to_path_buf()]),
        ..TestOptions::default()
    })
    .await
}

#[tokio::test]
async fn the_folders_mounted_into_the_container_are_listed_for_the_picker() {
    let app = app_browsing_the_temp_root().await;
    let root = app.dir.path().canonicalize().unwrap();
    for d in ["media", "temp", "output", "data", "web", "plain"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    // A media share with a drive mounted inside it, a work and an output
    // folder, the settings folder and the app's own web folder.
    let _mounts = [
        root.join("media"),
        root.join("media/Movies/USB"),
        root.join("temp"),
        root.join("output"),
        root.join("data"),
        root.join("web"),
    ]
    .map(|p| hang::mount(&p));

    let r = app.get("/api/fs/browse").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // Outermost mounts only, the app's own folder left out, sorted by path.
    assert_eq!(
        user_folder_paths(&r.json),
        [
            root.join("data"),
            root.join("media"),
            root.join("output"),
            root.join("temp"),
        ]
        .map(|p| p.to_str().unwrap().to_string()),
        "{}",
        r.text
    );
    let folders = r.json["user_folders"].as_array().unwrap();
    assert_eq!(folders[1]["name"], "media");
    // The settings folder can't be a library; the others can.
    assert!(
        folders[0]["library_blocked"]
            .as_str()
            .unwrap()
            .contains("keeps its database and settings"),
        "{}",
        r.text
    );
    for f in &folders[1..] {
        assert!(f.get("library_blocked").is_none(), "{f}");
    }
    // A folder that isn't a system folder is listed as before.
    let plain = r.json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "plain")
        .unwrap();
    assert!(plain.get("system").is_none(), "{plain}");
    // Every folder answers with the same list, wherever it is browsed.
    let r = app
        .get(&format!("/api/fs/browse?path={}", enc(&root.join("media"))))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(user_folder_paths(&r.json).len(), 4, "{}", r.text);
}

#[tokio::test]
async fn mounts_outside_the_browse_roots_are_not_offered() {
    // The default root is the media folder only: the picker would refuse
    // the work folder next to it, so it isn't offered.
    let app = TestApp::new().await;
    let root = app.dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("temp")).unwrap();
    let _media = hang::mount(&root.join("media"));
    let _temp = hang::mount(&root.join("temp"));
    let r = app.get("/api/fs/browse").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        user_folder_paths(&r.json),
        [root.join("media").to_str().unwrap()],
        "{}",
        r.text
    );
}

#[tokio::test]
async fn a_container_with_nothing_mounted_lists_no_folders_of_its_own() {
    let app = app_browsing_the_temp_root().await;
    let r = app.get("/api/fs/browse").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["user_folders"], serde_json::json!([]), "{}", r.text);
}

#[tokio::test]
async fn the_containers_own_folders_are_marked_as_system_folders() {
    let app = TestApp::start(TestOptions {
        configure: Box::new(|c, _| c.browse_roots = vec![std::path::PathBuf::from("/")]),
        ..TestOptions::default()
    })
    .await;
    let r = app.get("/api/fs/browse?path=/").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let entries = r.json["entries"].as_array().unwrap();
    let marked = |name: &str| {
        entries
            .iter()
            .find(|e| e["name"] == name)
            .map(|e| e.get("system") == Some(&serde_json::json!(true)))
    };
    // Whatever the machine has of these is a system folder (a link such as
    // /bin, which leads into /usr, as well).
    for name in ["etc", "usr", "proc", "dev", "sys", "bin", "lib", "var"] {
        if let Some(system) = marked(name) {
            assert!(system, "{name}: {}", r.text);
        }
    }
    // Folders of the machine's own are left as they were.
    for name in ["tmp", "home", "mnt", "opt"] {
        if let Some(system) = marked(name) {
            assert!(!system, "{name}: {}", r.text);
        }
    }
}
