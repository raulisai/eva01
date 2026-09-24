//! The speech models EVA01 knows how to install, and where it looks for the
//! one to use (`docs/PLAN.md` §5, §8: "1-2 modelos fijados alcanzan" — no
//! catalog, no download manager, just the pinned files and a way to check
//! they are all there).
//!
//! The file lists, sizes and SHA-256 sums were read off the Hugging Face
//! repositories' own API, and every URL names an exact revision, not `main`:
//! a repository that later changes a file must not break every install (the
//! size check would reject the new file) or, worse, swap the model for one
//! nobody measured. The sums were checked against the files installed and
//! verified on this Mac. `nemo128.onnx`, the mel-spectrogram preprocessor every
//! NeMo-family model needs, is not in the Canary repositories although
//! `transcribe-rs` requires it beside them — it ships in the Parakeet one
//! and is the same file, which is why it is downloaded from there.

use crate::{expand_home, Config};
use std::path::{Path, PathBuf};

/// One file of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
    /// Where to download it from.
    pub url: &'static str,
    /// Its name inside the model's folder.
    pub name: &'static str,
    /// Its exact size in bytes, used to tell a finished download from a
    /// partial one.
    pub bytes: u64,
    /// Its SHA-256, lowercase hex: what tells a corrupt or tampered download
    /// from the real file.
    pub sha256: &'static str,
}

/// A model EVA01 can install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    /// What to type after `eva model install`, and the folder's name.
    pub id: &'static str,
    /// One line for `eva model list`.
    pub description: &'static str,
    /// Every file it needs.
    pub files: &'static [ModelFile],
}

impl ModelSpec {
    /// The download size of all files, in bytes.
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }

    /// The files not yet present in `dir` with their full size.
    pub fn missing_files(&self, dir: &Path) -> Vec<&ModelFile> {
        self.files
            .iter()
            .filter(|file| std::fs::metadata(dir.join(file.name)).map_or(true, |meta| meta.len() != file.bytes))
            .collect()
    }

    /// Whether `dir` holds the whole model.
    pub fn is_installed(&self, dir: &Path) -> bool {
        self.missing_files(dir).is_empty()
    }
}

const NEMO_PREPROCESSOR: ModelFile = ModelFile {
    url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce/nemo128.onnx",
    name: "nemo128.onnx",
    bytes: 139_764,
    sha256: "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f",
};

/// The models EVA01 can install, best first. `canary-1b-flash` is the
/// production default (native Spanish, `docs/PLAN.md` §5); the 180m variant
/// is the same code path at a fifth of the size, for a quick start or a
/// small disk.
pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "canary-1b-flash",
        description: "Canary 1B Flash (int8) — español nativo, el modelo recomendado (~940 MB)",
        files: &[
            ModelFile {
                url: "https://huggingface.co/istupakov/canary-1b-flash-onnx/resolve/8876c67043cbb9cbcb69995cf3ea005709a51187/encoder-model.int8.onnx",
                name: "encoder-model.int8.onnx",
                bytes: 859_379_461,
                sha256: "0c2e89b23e8a72c789c80ce81ac5c13ee4a5a4c9b7c0ff2ded3a62a31e394f04",
            },
            ModelFile {
                url: "https://huggingface.co/istupakov/canary-1b-flash-onnx/resolve/8876c67043cbb9cbcb69995cf3ea005709a51187/decoder-model.int8.onnx",
                name: "decoder-model.int8.onnx",
                bytes: 79_520_498,
                sha256: "6c2d07674923fd2e1f57b217ed0886d807c60c7b51a3adc984fd14ca56ccbbc2",
            },
            ModelFile {
                url: "https://huggingface.co/istupakov/canary-1b-flash-onnx/resolve/8876c67043cbb9cbcb69995cf3ea005709a51187/vocab.txt",
                name: "vocab.txt",
                bytes: 53_566,
                sha256: "299c1538c63570a5a73fb7fa9ae29927991e8b2e9d179cbfd42d5f683f0273ec",
            },
            NEMO_PREPROCESSOR,
        ],
    },
    ModelSpec {
        id: "canary-180m-flash",
        description: "Canary 180M Flash (int8) — la misma familia, más pequeño y rápido (~214 MB)",
        files: &[
            ModelFile {
                url: "https://huggingface.co/istupakov/canary-180m-flash-onnx/resolve/92c2231a4e2b2524277fea759be967d2e6edfc49/encoder-model.int8.onnx",
                name: "encoder-model.int8.onnx",
                bytes: 133_710_896,
                sha256: "996d1c89e6cbc891a7c88bf410884c178ffa474f7b13084522ac74a5e144cc81",
            },
            ModelFile {
                url: "https://huggingface.co/istupakov/canary-180m-flash-onnx/resolve/92c2231a4e2b2524277fea759be967d2e6edfc49/decoder-model.int8.onnx",
                name: "decoder-model.int8.onnx",
                bytes: 79_520_211,
                sha256: "9dd9c447872088c912e916d73751f9621a54085d5bc46788454fe904db51a914",
            },
            ModelFile {
                url: "https://huggingface.co/istupakov/canary-180m-flash-onnx/resolve/92c2231a4e2b2524277fea759be967d2e6edfc49/vocab.txt",
                name: "vocab.txt",
                bytes: 53_555,
                sha256: "2dae6fc7815f9640645e0c765522b278ee0cef49b482d91f6913e334628d3e77",
            },
            NEMO_PREPROCESSOR,
        ],
    },
];

/// Looks a model up by id.
pub fn find(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

/// Where model `id` lives inside `support`.
pub fn model_dir(support: &Path, id: &str) -> PathBuf {
    support.join("models").join(id)
}

/// Which speech model to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChoice {
    /// A Canary model directory.
    Canary(PathBuf),
    /// A Whisper `ggml-*.bin` file.
    Whisper(PathBuf),
    /// Nothing usable was found.
    None,
}

/// Looks for a speech model, in order: the config file, the environment
/// (`EVA_CANARY_MODEL_DIR` / `EVA_STT_MODEL_PATH`, kept for development and
/// for `eva-eval`), then the standard place inside the support directory
/// (`models/canary-1b-flash`, `models/canary-180m-flash`, any `*.bin`). A
/// packaged app launched from Finder has no environment, so the config file
/// and the standard folder are what real use runs on. Canary is preferred:
/// `docs/PLAN.md` §5's production default, native Spanish.
///
/// A Canary folder counts only if it holds the whole model — a half-finished
/// download must not be picked and then fail to load.
pub fn discover(config: &Config, support: &Path, env: &dyn Fn(&str) -> Option<String>) -> ModelChoice {
    let standard = MODELS.iter().map(|m| model_dir(support, m.id).to_string_lossy().into_owned());
    let candidates = [config.stt.canary_dir.clone(), env("EVA_CANARY_MODEL_DIR")].into_iter().flatten().chain(standard);
    for candidate in candidates {
        let path = expand_home(&candidate);
        if path.is_dir() && looks_like_canary(&path) {
            return ModelChoice::Canary(path);
        }
    }

    for candidate in [config.stt.whisper_path.clone(), env("EVA_STT_MODEL_PATH")].into_iter().flatten() {
        let path = expand_home(&candidate);
        if path.is_file() {
            return ModelChoice::Whisper(path);
        }
    }
    let first_bin = std::fs::read_dir(support.join("models"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "bin"));
    first_bin.map_or(ModelChoice::None, ModelChoice::Whisper)
}

/// The files `transcribe-rs` needs to load a Canary model with int8
/// quantization.
fn looks_like_canary(dir: &Path) -> bool {
    ["encoder-model.int8.onnx", "decoder-model.int8.onnx", "vocab.txt", "nemo128.onnx"]
        .iter()
        .all(|name| dir.join(name).is_file())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// A support dir with the named model folders (complete Canary
    /// installs) and loose files.
    fn support_with(canary: &[&str], files: &[&str]) -> tempfile::TempDir {
        let support = tempfile::tempdir().expect("tempdir");
        for id in canary {
            let dir = support.path().join("models").join(id);
            std::fs::create_dir_all(&dir).expect("mkdir");
            for name in ["encoder-model.int8.onnx", "decoder-model.int8.onnx", "vocab.txt", "nemo128.onnx"] {
                std::fs::write(dir.join(name), "x").expect("write");
            }
        }
        for file in files {
            let path = support.path().join(file);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, "x").expect("write");
        }
        support
    }

    #[test]
    fn nothing_installed_finds_nothing() {
        let support = support_with(&[], &[]);
        assert_eq!(discover(&Config::default(), support.path(), &no_env), ModelChoice::None);
    }

    #[test]
    fn the_standard_folder_is_found_without_any_configuration() {
        let support = support_with(&["canary-1b-flash"], &[]);
        assert_eq!(
            discover(&Config::default(), support.path(), &no_env),
            ModelChoice::Canary(support.path().join("models/canary-1b-flash"))
        );
    }

    #[test]
    fn the_big_canary_is_preferred_over_the_small_one() {
        let support = support_with(&["canary-1b-flash", "canary-180m-flash"], &[]);
        assert_eq!(
            discover(&Config::default(), support.path(), &no_env),
            ModelChoice::Canary(support.path().join("models/canary-1b-flash"))
        );
    }

    #[test]
    fn a_half_downloaded_folder_is_not_picked() {
        let support = support_with(&["canary-180m-flash"], &[]);
        let broken = support.path().join("models/canary-1b-flash");
        std::fs::create_dir_all(&broken).expect("mkdir");
        std::fs::write(broken.join("vocab.txt"), "x").expect("write");

        assert_eq!(
            discover(&Config::default(), support.path(), &no_env),
            ModelChoice::Canary(support.path().join("models/canary-180m-flash")),
            "an incomplete 1B must not shadow a complete 180M"
        );
    }

    #[test]
    fn the_config_file_wins_over_the_environment_and_the_standard_folder() {
        let support = support_with(&["canary-1b-flash"], &[]);
        let elegido = support_with(&["x"], &[]);
        let elegido_dir = elegido.path().join("models/x");
        let mut config = Config::default();
        config.stt.canary_dir = Some(elegido_dir.to_string_lossy().into_owned());
        assert_eq!(discover(&config, support.path(), &no_env), ModelChoice::Canary(elegido_dir));
    }

    #[test]
    fn the_environment_wins_over_the_standard_folder() {
        let support = support_with(&["canary-1b-flash"], &[]);
        let del_entorno = support_with(&["y"], &[]);
        let dir = del_entorno.path().join("models/y");
        let env_dir = dir.to_string_lossy().into_owned();
        let env = move |key: &str| (key == "EVA_CANARY_MODEL_DIR").then(|| env_dir.clone());
        assert_eq!(discover(&Config::default(), support.path(), &env), ModelChoice::Canary(dir));
    }

    #[test]
    fn a_configured_path_that_does_not_exist_is_skipped_not_trusted() {
        let support = support_with(&["canary-180m-flash"], &[]);
        let mut config = Config::default();
        config.stt.canary_dir = Some("/no/existe/canary".to_string());
        assert_eq!(
            discover(&config, support.path(), &no_env),
            ModelChoice::Canary(support.path().join("models/canary-180m-flash"))
        );
    }

    #[test]
    fn whisper_is_the_fallback_when_no_canary_exists() {
        let support = support_with(&[], &["models/ggml-base.bin"]);
        assert_eq!(
            discover(&Config::default(), support.path(), &no_env),
            ModelChoice::Whisper(support.path().join("models/ggml-base.bin"))
        );
    }

    #[test]
    fn canary_beats_whisper_when_both_are_installed() {
        let support = support_with(&["canary-180m-flash"], &["models/ggml-base.bin"]);
        assert!(matches!(discover(&Config::default(), support.path(), &no_env), ModelChoice::Canary(_)));
    }

    #[test]
    fn an_environment_whisper_path_is_honored() {
        let support = support_with(&[], &["elsewhere/modelo.bin"]);
        let path = support.path().join("elsewhere/modelo.bin");
        let env_path = path.to_string_lossy().into_owned();
        let env = move |key: &str| (key == "EVA_STT_MODEL_PATH").then(|| env_path.clone());
        assert_eq!(discover(&Config::default(), support.path(), &env), ModelChoice::Whisper(path));
    }

    // ---- the manifest ----

    #[test]
    fn every_model_lists_the_four_files_the_loader_needs_and_no_duplicates() {
        for model in MODELS {
            let names: Vec<_> = model.files.iter().map(|f| f.name).collect();
            for needed in ["encoder-model.int8.onnx", "decoder-model.int8.onnx", "vocab.txt", "nemo128.onnx"] {
                assert!(names.contains(&needed), "{} lacks {needed}", model.id);
            }
            let mut unique = names.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), names.len(), "{} lists a file twice", model.id);
        }
    }

    #[test]
    fn every_url_is_https_and_ends_with_its_file_name() {
        for file in MODELS.iter().flat_map(|m| m.files) {
            assert!(file.url.starts_with("https://"), "{}", file.url);
            assert!(file.url.ends_with(file.name), "{} does not end with {}", file.url, file.name);
        }
    }

    #[test]
    fn every_file_is_pinned_to_a_revision_and_a_checksum() {
        for file in MODELS.iter().flat_map(|m| m.files) {
            assert!(!file.url.contains("/resolve/main/"), "{} follows a branch, not a revision", file.url);
            assert_eq!(file.sha256.len(), 64, "{}", file.name);
            assert!(file.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{}", file.name);
        }
    }

    #[test]
    fn the_recommended_model_comes_first_and_ids_resolve() {
        assert_eq!(MODELS[0].id, "canary-1b-flash");
        assert_eq!(find("canary-180m-flash").map(|m| m.id), Some("canary-180m-flash"));
        assert!(find("no-existe").is_none());
    }

    #[test]
    fn missing_files_are_those_absent_or_with_the_wrong_size() {
        let model = find("canary-180m-flash").expect("known");
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(model.missing_files(dir.path()).len(), model.files.len());

        // A file of the wrong size is a partial download, not an install.
        std::fs::write(dir.path().join("vocab.txt"), "corto").expect("write");
        assert!(model.missing_files(dir.path()).iter().any(|f| f.name == "vocab.txt"));

        std::fs::write(dir.path().join("vocab.txt"), vec![b'x'; 53_555]).expect("write");
        assert!(!model.missing_files(dir.path()).iter().any(|f| f.name == "vocab.txt"));
        assert!(!model.is_installed(dir.path()));
    }

    #[test]
    fn totals_match_the_advertised_sizes() {
        let big = find("canary-1b-flash").expect("known").total_bytes();
        let small = find("canary-180m-flash").expect("known").total_bytes();
        assert!((900_000_000..1_000_000_000).contains(&big), "{big}");
        assert!((200_000_000..230_000_000).contains(&small), "{small}");
    }
}
