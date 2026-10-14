//! Port of test/unit/test_cdb_samples.cpp.

use std::path::PathBuf;

use symhow::index::arg_sanitizer::{sanitize, SanitizerOptions};
use symhow::index::compile_db::{load, CompileDbOptions};

/// The fixture lives at test/integration/data/cucs_cdb_samples.json, i.e.
/// ../cpp/test/integration/data relative to the crate root (rust/), or resolved
/// from SYMHOW_TEST_SOURCE_DIR when set.
fn samples_path() -> PathBuf {
    if let Ok(dir) = std::env::var("SYMHOW_TEST_SOURCE_DIR") {
        return PathBuf::from(dir)
            .join("integration")
            .join("data")
            .join("cucs_cdb_samples.json");
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("test")
        .join("integration")
        .join("data")
        .join("cucs_cdb_samples.json")
}

/// Writes `content` to a fresh scratch file under the system temp dir.
fn write_scratch_cdb(name: &str, content: &str) -> PathBuf {
    let dir = std::env::temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn cucs_cdb_samples_full_remap_sanitize_pipeline() {
    let mut db_options = CompileDbOptions::default();
    db_options
        .remaps
        .push(("/mnt/workspace".to_string(), "/home/aaramoon/workspace".to_string()));

    let entries = load(samples_path().to_str().unwrap(), &db_options).unwrap();
    assert_eq!(entries.len(), 3);

    let sanitizer_options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(),
        ..Default::default()
    };

    let mut cu_count = 0;
    for entry in &entries {
        // No stale /mnt/workspace prefix anywhere, including inside directory/file.
        assert!(!entry.directory.contains("/mnt/workspace"), "file: {}", entry.file);
        assert!(!entry.file.contains("/mnt/workspace"), "file: {}", entry.file);

        let result = sanitize(entry, &sanitizer_options);

        for tok in &result.args {
            assert!(!tok.contains("/mnt/workspace"), "token: {}", tok);
        }

        // None of these should ever survive sanitizing.
        for tok in &result.args {
            assert!(!tok.contains("-fmodules-ts"));
            assert!(!tok.contains("-fmodule-mapper"));
            assert!(!tok.contains("-fdeps-format"));
            assert!(!tok.contains("forward-unknown"));
        }

        if entry.file.ends_with(".cu") {
            cu_count += 1;
            let mut saw_x_cuda = false;
            for i in 0..result.args.len().saturating_sub(1) {
                if result.args[i] == "-x" && result.args[i + 1] == "cuda" {
                    saw_x_cuda = true;
                    break;
                }
            }
            assert!(saw_x_cuda);
        }
    }

    assert_eq!(cu_count, 1);
}

#[test]
fn load_supports_the_already_tokenized_arguments_array_form() {
    let json = r#"[
        {
            "directory": "/work",
            "file": "/work/foo.cpp",
            "arguments": ["/usr/bin/c++", "-DFOO=1", "-I/work/include", "-c", "foo.cpp", "-o", "foo.o"]
        }
    ]"#;
    let path = write_scratch_cdb("arguments_form.json", json);

    let db_options = CompileDbOptions::default();
    let entries = load(path.to_str().unwrap(), &db_options).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].directory, "/work");
    assert_eq!(entries[0].file, "/work/foo.cpp");
    assert!(entries[0].output.is_empty());
    let expected: Vec<String> = vec![
        "/usr/bin/c++",
        "-DFOO=1",
        "-I/work/include",
        "-c",
        "foo.cpp",
        "-o",
        "foo.o",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(entries[0].args, expected);
}

#[test]
fn load_applies_remap_to_the_output_field_too() {
    let json = r#"[
        {
            "directory": "/mnt/workspace/build",
            "file": "/mnt/workspace/src/foo.cpp",
            "output": "/mnt/workspace/build/foo.o",
            "arguments": ["/usr/bin/c++", "-c", "/mnt/workspace/src/foo.cpp"]
        }
    ]"#;
    let path = write_scratch_cdb("output_remap.json", json);

    let mut db_options = CompileDbOptions::default();
    db_options
        .remaps
        .push(("/mnt/workspace".to_string(), "/home/aaramoon/workspace".to_string()));
    let entries = load(path.to_str().unwrap(), &db_options).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].output, "/home/aaramoon/workspace/build/foo.o");
}

#[test]
fn load_include_exclude_filter_on_the_post_remap_file_path() {
    let json = r#"[
        {"directory": "/w", "file": "/mnt/workspace/src/foo.cpp", "arguments": ["/usr/bin/c++", "foo.cpp"]},
        {"directory": "/w", "file": "/mnt/workspace/test/bar.cpp", "arguments": ["/usr/bin/c++", "bar.cpp"]}
    ]"#;
    let path = write_scratch_cdb("include_exclude.json", json);

    let mut db_options = CompileDbOptions::default();
    db_options
        .remaps
        .push(("/mnt/workspace".to_string(), "/home/aaramoon/workspace".to_string()));
    db_options
        .include_globs
        .push("/home/aaramoon/workspace/src/**".to_string());
    let entries = load(path.to_str().unwrap(), &db_options).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].file, "/home/aaramoon/workspace/src/foo.cpp");
}