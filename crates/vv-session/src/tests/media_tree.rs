use super::*;
use crate::test_support::TempDir;

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"").unwrap();
}

#[test]
fn a_folder_becomes_a_tree_of_its_media_only() {
    let root = TempDir::new("vv-session-media-tree");
    let shots = root.join("Shots");
    for file in [
        "a.mp4",
        "notes.txt",
        ".hidden.mp4",
        "Day 1/b.MOV",
        "Day 1/Audio/c.wav",
        "Empty/readme.md",
    ] {
        touch(&shots.join(file));
    }
    let loose = root.join("loose.txt");
    touch(&loose);

    let tree = MediaTree::scan(&[shots.clone(), loose.clone()]);

    assert_eq!(
        tree.folders,
        [
            ("Shots".to_string(), None),
            ("Day 1".to_string(), Some(0)),
            ("Audio".to_string(), Some(1)),
        ]
    );
    assert_eq!(
        tree.files,
        [
            (shots.join("Day 1/Audio/c.wav"), Some(2)),
            (shots.join("Day 1/b.MOV"), Some(1)),
            (shots.join("a.mp4"), Some(0)),
            (loose, None),
        ],
        "a dropped file goes in whatever it is, the rest by extension"
    );
}

#[test]
fn a_folder_without_media_brings_nothing() {
    let root = TempDir::new("vv-session-media-tree-empty");
    touch(&root.join("Docs/a.txt"));
    assert_eq!(MediaTree::scan(&[root.join("Docs")]), MediaTree::default());
}
