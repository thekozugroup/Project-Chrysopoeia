//! `/api/fs/browse`: listing and traversal attempts.

use axum::http::StatusCode;

use super::support::*;

fn enc(p: &std::path::Path) -> String {
    p.to_str()
        .unwrap()
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('#', "%23")
}

#[tokio::test]
async fn lists_folders_sorted_without_hidden_ones() {
    let app = TestApp::new().await;
    for d in ["movies", "Anime", "TV Shows", ".hidden", "zeta"] {
        std::fs::create_dir_all(app.media.join(d)).unwrap();
    }
    app.write("TV Shows/a.mkv", h264());
    app.write("TV Shows/b.mp4", h264());
    app.write("TV Shows/readme.txt", "x");
    app.write("loose.mkv", h264());

    let r = app.get("/api/fs/browse").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let root = std::fs::canonicalize(&app.media).unwrap();
    assert_eq!(r.json["path"], root.to_str().unwrap());
    assert!(r.json["parent"].is_null(), "no parent above a root");
    assert_eq!(r.json["roots"][0], root.to_str().unwrap());
    let names: Vec<&str> = r.json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Anime", "movies", "TV Shows", "zeta"]);
    let tv = &r.json["entries"][2];
    assert_eq!(tv["is_dir"], true);
    assert_eq!(tv["media_count"], 2);
    assert_eq!(tv["path"], root.join("TV Shows").to_str().unwrap());

    let r = app
        .get(&format!(
            "/api/fs/browse?path={}",
            enc(&root.join("TV Shows"))
        ))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["parent"], root.to_str().unwrap());
    assert_eq!(r.json["entries"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn traversal_and_bad_paths_are_refused() {
    let app = TestApp::new().await;
    let root = std::fs::canonicalize(&app.media).unwrap();
    std::fs::create_dir_all(root.join("inside")).unwrap();
    let outside = app.dir.path().join("secret");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("key.txt"), "x").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("inside"), root.join("shortcut")).unwrap();

    for path in [
        format!("{}/../../etc", root.display()),
        format!("{}/../secret", root.display()),
        "/etc".to_string(),
        "/".to_string(),
        format!("{}/escape", root.display()),
    ] {
        let r = app
            .get(&format!("/api/fs/browse?path={}", path.replace(' ', "%20")))
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.text);
        assert_eq!(r.json["code"], "outside_roots", "{path}");
    }

    // The symlink escaping the root is not listed; one inside is.
    let r = app.get("/api/fs/browse").await;
    let names: Vec<&str> = r.json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"escape"), "{names:?}");
    #[cfg(unix)]
    assert!(names.contains(&"shortcut"), "{names:?}");

    let r = app
        .get(&format!(
            "/api/fs/browse?path={}",
            enc(&root.join("missing"))
        ))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json["code"], "path_not_found");
    let r = app.get("/api/fs/browse?path=relative/dir").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json["code"], "path_not_absolute");
    app.write("file.mkv", "x");
    let r = app
        .get(&format!(
            "/api/fs/browse?path={}",
            enc(&root.join("file.mkv"))
        ))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json["code"], "not_a_directory");
}
