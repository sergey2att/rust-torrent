//! Тесты `DiskStorage`: граница двух файлов, pre-allocation, последний
//! короткий кусок, санитизация путей, чтение обратно.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation
)] // тесты вправе паниковать

use engine::{DiskStorage, EngineError};
use metainfo::{FileEntry, FileMode, Info};
use sha1::{Digest, Sha1};
use std::path::Path;

/// `Info` валидируется через SHA-1, как настоящий .torrent.
fn info_single(name: &str, piece_length: u64, total: u64) -> Info {
    let count = usize::try_from(total.div_ceil(piece_length)).unwrap();
    let mut pieces = Vec::with_capacity(count);
    for i in 0..count {
        let len = (total - i as u64 * piece_length).min(piece_length);
        let data = vec![i as u8; usize::try_from(len).unwrap()]; // piece_count < 256 в фикстурах
        pieces.push(Sha1::digest(&data).into());
    }
    Info {
        piece_length,
        pieces,
        name: name.to_string(),
        mode: FileMode::Single { length: total },
    }
}

fn info_multi(name: &str, files: &[(Vec<&str>, u64)]) -> Info {
    // Куски по границе файлов: считаем SHA-1 от конкатенации.
    let piece_length = 64u64;
    let data: Vec<u8> = files
        .iter()
        .flat_map(|f| vec![b'x'; usize::try_from(f.1).unwrap()])
        .collect();
    let pieces = data
        .chunks(usize::try_from(piece_length).unwrap())
        .map(|c| Sha1::digest(c).into())
        .collect();
    Info {
        piece_length,
        pieces,
        name: name.to_string(),
        mode: FileMode::Multi {
            files: files
                .iter()
                .map(|f| FileEntry {
                    path: f.0.iter().map(ToString::to_string).collect(),
                    length: f.1,
                })
                .collect(),
        },
    }
}

#[test]
fn single_file_write_and_read_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let info = info_single("file.bin", 64, 200); // 4 куска: 64+64+64+8
    let mut storage = DiskStorage::new(&info, dir.path()).unwrap();

    let piece0 = vec![0u8; 64];
    let piece3 = vec![3u8; 8];
    storage.write_piece(0, &piece0).unwrap();
    storage.write_piece(3, &piece3).unwrap();

    assert_eq!(storage.read_piece(0).unwrap(), piece0);
    assert_eq!(storage.read_piece(3).unwrap(), piece3);
    // Pre-allocation: файл уже полной длины.
    let meta = std::fs::metadata(dir.path().join("file.bin")).unwrap();
    assert_eq!(meta.len(), 200);
    // Незаписанные куски — нули (sparse).
    assert!(storage.read_piece(1).unwrap().iter().all(|&b| b == 0));
    assert_eq!(storage.file_paths(), vec![dir.path().join("file.bin")]);
}

#[test]
fn write_piece_across_file_boundary_splits_data() {
    let dir = tempfile::tempdir().unwrap();
    // Файлы 10 и 20 байт; кусок 16 байт лежит на границе (10 + 6).
    let info = info_multi("multi", &[(vec!["a.bin"], 10), (vec!["b.bin"], 20)]);
    let mut storage = DiskStorage::new(&info, dir.path()).unwrap();
    assert_eq!(info.total_length(), 30);
    assert_eq!(info.piece_count(), 1); // один кусок 30 байт? нет: 64-байтные куски → 1 кусок

    let piece = vec![b'x'; 30];
    storage.write_piece(0, &piece).unwrap();
    let a = std::fs::read(dir.path().join("multi").join("a.bin")).unwrap();
    let b = std::fs::read(dir.path().join("multi").join("b.bin")).unwrap();
    assert_eq!(a.len(), 10);
    assert_eq!(b.len(), 20);
    assert!(a.iter().all(|&c| c == b'x'));
    assert!(b.iter().all(|&c| c == b'x'));
    assert_eq!(storage.read_piece(0).unwrap(), piece);
}

#[test]
fn multi_file_piece_touching_three_files() {
    let dir = tempfile::tempdir().unwrap();
    // Файлы 8, 8, 8; кусок 64 не влезет, поэтому piece_length 24 — 1 кусок на 3 файла.
    let mut info = info_multi("m3", &[(vec!["1"], 8), (vec!["2"], 8), (vec!["3"], 8)]);
    info.piece_length = 24;
    // Пересчитываем pieces под piece_length 24 (3 файла по 8 = 24 = 1 кусок).
    info.pieces = vec![Sha1::digest([b'x'; 24]).into()];
    let mut storage = DiskStorage::new(&info, dir.path()).unwrap();
    storage.write_piece(0, &[b'x'; 24]).unwrap();
    for name in ["1", "2", "3"] {
        let data = std::fs::read(dir.path().join("m3").join(name)).unwrap();
        assert_eq!(data, vec![b'x'; 8]);
    }
    assert_eq!(storage.read_piece(0).unwrap(), [b'x'; 24]);
}

#[test]
fn multi_file_creates_subdirectories_and_full_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let info = info_multi(
        "sub",
        &[(vec!["dir1", "one.bin"], 100), (vec!["two.bin"], 50)],
    );
    let storage = DiskStorage::new(&info, dir.path()).unwrap();
    let one = dir.path().join("sub").join("dir1").join("one.bin");
    let two = dir.path().join("sub").join("two.bin");
    assert_eq!(std::fs::metadata(&one).unwrap().len(), 100);
    assert_eq!(std::fs::metadata(&two).unwrap().len(), 50);
    assert_eq!(storage.file_paths(), vec![one, two]);
}

#[test]
fn write_piece_beyond_total_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let info = info_single("f", 64, 64);
    let mut storage = DiskStorage::new(&info, dir.path()).unwrap();
    let err = storage.write_piece(5, &[0u8; 8]).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn unsafe_path_components_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let cases: Vec<Info> = vec![
        // Пустой компонент.
        info_multi("n", &[(vec![""], 10)]),
        // Родительский каталог.
        info_multi("n", &[(vec![".."], 10)]),
        // Вложенный выход за пределы.
        info_multi("n", &[(vec!["ok", "..", "evil"], 10)]),
        // Текущий каталог.
        info_multi("n", &[(vec!["."], 10)]),
        // Слэш в компоненте.
        info_multi("n", &[(vec!["a/b"], 10)]),
        // Обратный слэш.
        info_multi("n", &[(vec!["a\\b"], 10)]),
        // Абсолютный путь.
        info_multi("n", &[(vec!["/etc"], 10)]),
    ];
    for (i, info) in cases.into_iter().enumerate() {
        let err = DiskStorage::new(&info, dir.path().join(format!("case{i}")).as_path())
            .expect_err("небезопасный путь обязан быть отвергнут");
        assert!(matches!(err, EngineError::UnsafePath(_)));
    }
}

#[test]
fn unsafe_root_name_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut info = info_single("ok.bin", 64, 64);
    info.name = "../escape".to_string();
    let err = DiskStorage::new(&info, dir.path()).unwrap_err();
    assert!(matches!(err, EngineError::UnsafePath(_)));

    let mut info = info_single("ok.bin", 64, 64);
    info.name = String::new();
    assert!(matches!(
        DiskStorage::new(&info, dir.path()),
        Err(EngineError::UnsafePath(_))
    ));
}

#[test]
fn valid_nested_paths_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let info = info_multi("t", &[(vec!["ру-с", "файл.bin"], 10)]);
    let storage = DiskStorage::new(&info, dir.path()).unwrap();
    assert_eq!(storage.file_paths().len(), 1);
    assert!(Path::new(&storage.file_paths()[0]).exists());
}
