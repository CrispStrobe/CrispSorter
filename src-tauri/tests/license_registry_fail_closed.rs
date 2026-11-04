// Exercise the actual gate without downloading models. Each stateful scenario runs
// in a fresh process so global acceptance and the caller's environment cannot
// affect another test.
#[path = "../src/index/license_consent.rs"]
mod license_consent;

use license_consent::{ensure_for_registry_name, license_for_registry_name, ModelLicense};

fn isolated(test: &str, env_value: Option<&str>, body: impl FnOnce()) {
    const CHILD: &str = "CRISPSORTER_TEST_LICENSE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        body();
        return;
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, "1")
        .env_remove("CRISPSORTER_ACCEPT_MODEL_LICENSE");
    if let Some(value) = env_value {
        command.env("CRISPSORTER_ACCEPT_MODEL_LICENSE", value);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unknown_registry_names_are_never_authorized_by_consent() {
    isolated(
        "unknown_registry_names_are_never_authorized_by_consent",
        None,
        || {
            for name in [
                "",
                "test-unreviewed-registry-model-unique",
                "bge-m3-unknown-quant",
                "BGE-M3",
                "xlmr-ner-hrl",
            ] {
                let err = ensure_for_registry_name(name).unwrap_err().to_string();
                assert!(
                    err.contains("unknown") || err.contains("Unknown"),
                    "{name}: {err}"
                );
                license_consent::accept(name);
                assert!(ensure_for_registry_name(name).is_err(), "{name}");
            }
            license_consent::accept_all();
            assert!(ensure_for_registry_name("test-unreviewed-registry-model-unique").is_err());
        },
    );
}

#[test]
fn unknown_registry_names_ignore_environment_consent() {
    isolated(
        "unknown_registry_names_ignore_environment_consent",
        Some("all"),
        || {
            assert!(ensure_for_registry_name("test-unreviewed-registry-model-unique").is_err());
        },
    );
}

#[test]
fn known_permissive_models_remain_available() {
    isolated("known_permissive_models_remain_available", None, || {
        // Names from CrispEmbed's bundled model_mgr.cpp and the Settings mapping.
        for name in [
            "bge-m3",
            "pixie-rune-v1",
            "bge-m3-q8",
            "all-MiniLM-L6-v2",
            "harrier-0.6b",
            "gliner-deberta",
            "snowflake-arctic-lv2",
            "bge-small-en-v15",
            "esrgan-x4",
            "transcoda",
            "clip-vit-base-patch16",
        ] {
            assert_eq!(
                license_for_registry_name(name),
                ModelLicense::Permissive,
                "{name}"
            );
            assert!(ensure_for_registry_name(name).is_ok(), "{name}");
        }
    });
}

#[test]
fn restrictive_registry_variants_require_their_own_consent() {
    isolated(
        "restrictive_registry_variants_require_their_own_consent",
        None,
        || {
            for (name, expected) in [
                (
                    "jina-v5-nano-q8",
                    ModelLicense::NonCommercial("CC-BY-NC-4.0"),
                ),
                (
                    "jina-reranker-v2-base-multilingual-q4k",
                    ModelLicense::NonCommercial("CC-BY-NC-4.0"),
                ),
                ("splade-v3", ModelLicense::NonCommercial("CC-BY-NC-SA-4.0")),
                (
                    "embeddinggemma-300m-q4k",
                    ModelLicense::Restricted("Gemma Terms of Use"),
                ),
                (
                    "lfm2-embed-iq4xs",
                    ModelLicense::Restricted("LFM Open License v1.0"),
                ),
                (
                    "gliner-lfm-q4k",
                    ModelLicense::Restricted("LFM Open License v1.0"),
                ),
                (
                    "gemma3-embed-2b",
                    ModelLicense::Restricted("Gemma Terms of Use"),
                ),
                ("texo-distill", ModelLicense::Restricted("AGPL-3.0")),
                ("surya-det", ModelLicense::Restricted("OpenRAIL-M")),
            ] {
                assert_eq!(license_for_registry_name(name), expected, "{name}");
                assert!(ensure_for_registry_name(name).is_err(), "{name}");
                license_consent::accept(name);
                assert!(ensure_for_registry_name(name).is_ok(), "{name}");
            }
        },
    );
}

#[test]
fn blanket_consent_allows_known_restrictive_models() {
    isolated(
        "blanket_consent_allows_known_restrictive_models",
        None,
        || {
            assert!(ensure_for_registry_name("jina-v5-small").is_err());
            license_consent::accept_all();
            assert!(ensure_for_registry_name("jina-v5-small").is_ok());
            assert!(ensure_for_registry_name("embeddinggemma-300m").is_ok());
        },
    );
}
