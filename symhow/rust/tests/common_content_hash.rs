//! Port of test/unit/test_content_hash.cpp.

use symhow::common::content_hash::{content_hash, file_content_hash};

#[test]
fn sha256_standard_vectors_padding_boundaries_and_binary_bytes() {
    assert_eq!(
        content_hash(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        content_hash(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        content_hash(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    assert_eq!(
        content_hash(&vec![b'a'; 1_000_000]),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
    assert_eq!(
        content_hash(&vec![b'a'; 55]),
        "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
    );
    assert_eq!(
        content_hash(&vec![b'a'; 56]),
        "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
    );
    assert_eq!(
        content_hash(&vec![b'a'; 64]),
        "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
    );
    assert_eq!(
        content_hash(&vec![b'a'; 65]),
        "635361c48bb9eab14198e76ea8ab7f1a41685d6ad62aa9146d301d4f17eb0ae0"
    );
    assert_eq!(
        content_hash(&[0u8, 0xff, b'\r', b'\n']),
        "e9489f37fb3051e9efa1dc916004d7274e7b63975e3209708947267f2393a9be"
    );
    assert_ne!(content_hash(b"int a;\r\n"), content_hash(b"int a;\n"));
    assert_ne!(content_hash(b"int a;"), content_hash(b"int a;\n"));
}

#[test]
fn streamed_files_match_buffers_and_read_failures_are_conservative() {
    let dir = std::env::temp_dir().join(format!("symhow_content_hash_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("bytes");

    for size in [0usize, 1, 16383, 16384, 16385, 32768] {
        let bytes: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        std::fs::write(&file, &bytes).unwrap();
        assert_eq!(
            file_content_hash(file.to_str().unwrap()).unwrap(),
            content_hash(&bytes)
        );
    }

    assert!(file_content_hash(dir.join("missing").to_str().unwrap()).is_none());
    assert!(file_content_hash(dir.to_str().unwrap()).is_none());

    #[cfg(unix)]
    {
        let alias = dir.join("alias");
        std::os::unix::fs::symlink(&file, &alias).unwrap();
        assert_eq!(
            file_content_hash(alias.to_str().unwrap()),
            file_content_hash(file.to_str().unwrap())
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}