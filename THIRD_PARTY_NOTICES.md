# Avisos de terceros

EVA01 es MIT (ver `LICENSE`). Incluye o descarga lo siguiente. Generado por
`packaging/third-party.sh`; no lo edites a mano.

## Modelos de voz

`eva model install` descarga, desde Hugging Face, conversiones a ONNX de modelos de
NVIDIA, bajo la licencia [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/):

- Canary 1B Flash y Canary 180M Flash — © NVIDIA; conversión a ONNX (int8) de
  [istupakov](https://huggingface.co/istupakov).
- El preprocesador `nemo128.onnx` — de `parakeet-tdt-0.6b-v3` (© NVIDIA, CC BY 4.0),
  conversión de istupakov.

Los modelos no se distribuyen con EVA01: se descargan al instalarlos.

## Dependencias de Rust (311)

| Crate | Versión | Licencia |
|---|---|---|
| [adler2](https://github.com/oyvindln/adler2) | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| [aho-corasick](https://github.com/BurntSushi/aho-corasick) | 1.1.5 | Unlicense OR MIT |
| [anstream](https://github.com/rust-cli/anstyle.git) | 1.0.0 | MIT OR Apache-2.0 |
| [anstyle](https://github.com/rust-cli/anstyle.git) | 1.0.14 | MIT OR Apache-2.0 |
| [anstyle-parse](https://github.com/rust-cli/anstyle.git) | 1.0.0 | MIT OR Apache-2.0 |
| [anstyle-query](https://github.com/rust-cli/anstyle.git) | 1.1.5 | MIT OR Apache-2.0 |
| [async-trait](https://github.com/dtolnay/async-trait) | 0.1.92 | MIT OR Apache-2.0 |
| [audio-codec-algorithms](https://github.com/karip/audio-codec-algorithms) | 0.8.1 | 0BSD OR Apache-2.0 |
| [audioadapter](https://github.com/HEnquist/audioadapter-rs) | 5.0.0 | MIT OR Apache-2.0 |
| [audioadapter-buffers](https://github.com/HEnquist/audioadapter-buffers-rs) | 5.2.0 | MIT OR Apache-2.0 |
| [audioadapter-sample](https://github.com/HEnquist/audioadapter-sample-rs) | 5.2.0 | MIT OR Apache-2.0 |
| [autocfg](https://github.com/cuviper/autocfg) | 1.5.1 | Apache-2.0 OR MIT |
| [base64](https://github.com/marshallpierce/rust-base64) | 0.22.1 | MIT OR Apache-2.0 |
| [base64](https://github.com/marshallpierce/rust-base64) | 0.23.1 | MIT OR Apache-2.0 |
| [base64ct](https://github.com/RustCrypto/formats) | 1.8.3 | Apache-2.0 OR MIT |
| [bindgen](https://github.com/rust-lang/rust-bindgen) | 0.69.5 | BSD-3-Clause |
| [bit-set](https://github.com/contain-rs/bit-set) | 0.8.0 | Apache-2.0 OR MIT |
| [bit-vec](https://github.com/contain-rs/bit-vec) | 0.8.0 | Apache-2.0 OR MIT |
| [bitflags](https://github.com/bitflags/bitflags) | 1.3.2 | MIT/Apache-2.0 |
| [bitflags](https://github.com/bitflags/bitflags) | 2.13.2 | MIT OR Apache-2.0 |
| [block-buffer](https://github.com/RustCrypto/utils) | 0.10.4 | MIT OR Apache-2.0 |
| [block2](https://github.com/madsmtm/objc2) | 0.6.2 | MIT |
| [byteorder](https://github.com/BurntSushi/byteorder) | 1.5.0 | Unlicense OR MIT |
| [bytes](https://github.com/tokio-rs/bytes) | 1.12.1 | MIT |
| [cc](https://github.com/rust-lang/cc-rs) | 1.4.7 | MIT OR Apache-2.0 |
| [cexpr](https://github.com/jethrogb/rust-cexpr) | 0.6.0 | Apache-2.0/MIT |
| [cfg-if](https://github.com/rust-lang/cfg-if) | 1.0.5 | MIT OR Apache-2.0 |
| [chrono](https://github.com/chronotope/chrono) | 0.4.45 | MIT OR Apache-2.0 |
| [clang-sys](https://github.com/KyleMayes/clang-sys) | 1.9.1 | Apache-2.0 |
| [clap](https://github.com/clap-rs/clap) | 4.6.7 | MIT OR Apache-2.0 |
| [clap_builder](https://github.com/clap-rs/clap) | 4.6.7 | MIT OR Apache-2.0 |
| [clap_derive](https://github.com/clap-rs/clap) | 4.6.7 | MIT OR Apache-2.0 |
| [clap_lex](https://github.com/clap-rs/clap) | 1.1.1 | MIT OR Apache-2.0 |
| [cmake](https://github.com/rust-lang/cmake-rs) | 0.1.58 | MIT OR Apache-2.0 |
| [colorchoice](https://github.com/rust-cli/anstyle.git) | 1.0.5 | MIT OR Apache-2.0 |
| [cookie](https://github.com/SergioBenitez/cookie-rs) | 0.18.2 | MIT OR Apache-2.0 |
| [cookie_store](https://github.com/pfernie/cookie_store) | 0.22.1 | MIT OR Apache-2.0 |
| [core-foundation](https://github.com/servo/core-foundation-rs) | 0.10.1 | MIT OR Apache-2.0 |
| [core-foundation-sys](https://github.com/servo/core-foundation-rs) | 0.8.7 | MIT OR Apache-2.0 |
| [core-graphics](https://github.com/servo/core-foundation-rs) | 0.25.0 | MIT OR Apache-2.0 |
| [core-graphics-types](https://github.com/servo/core-foundation-rs) | 0.2.0 | MIT OR Apache-2.0 |
| [coreaudio-rs](https://github.com/RustAudio/coreaudio-rs.git) | 0.14.2 | MIT/Apache-2.0 |
| [cpal](https://github.com/RustAudio/cpal) | 0.18.2 | Apache-2.0 |
| [cpufeatures](https://github.com/RustCrypto/utils) | 0.2.17 | MIT OR Apache-2.0 |
| [crc32fast](https://github.com/srijs/rust-crc32fast) | 1.5.2 | MIT OR Apache-2.0 |
| [crossbeam-channel](https://github.com/crossbeam-rs/crossbeam) | 0.5.17 | MIT OR Apache-2.0 |
| [crossbeam-utils](https://github.com/crossbeam-rs/crossbeam) | 0.8.23 | MIT OR Apache-2.0 |
| [crypto-common](https://github.com/RustCrypto/traits) | 0.1.7 | MIT OR Apache-2.0 |
| [darling](https://github.com/TedDriggs/darling) | 0.20.11 | MIT |
| [darling](https://github.com/TedDriggs/darling) | 0.24.1 | MIT |
| [darling_core](https://github.com/TedDriggs/darling) | 0.20.11 | MIT |
| [darling_core](https://github.com/TedDriggs/darling) | 0.24.1 | MIT |
| [darling_macro](https://github.com/TedDriggs/darling) | 0.20.11 | MIT |
| [darling_macro](https://github.com/TedDriggs/darling) | 0.24.1 | MIT |
| [dasp_sample](https://github.com/rustaudio/sample.git) | 0.11.0 | MIT OR Apache-2.0 |
| [der](https://github.com/RustCrypto/formats) | 0.8.2 | Apache-2.0 OR MIT |
| [deranged](https://github.com/jhpratt/deranged) | 0.5.8 | MIT OR Apache-2.0 |
| [derive_builder](https://github.com/colin-kiegel/rust-derive-builder) | 0.20.2 | MIT OR Apache-2.0 |
| [derive_builder_core](https://github.com/colin-kiegel/rust-derive-builder) | 0.20.2 | MIT OR Apache-2.0 |
| [derive_builder_macro](https://github.com/colin-kiegel/rust-derive-builder) | 0.20.2 | MIT OR Apache-2.0 |
| [digest](https://github.com/RustCrypto/traits) | 0.10.7 | MIT OR Apache-2.0 |
| [dirs](https://github.com/soc/dirs-rs) | 5.0.1 | MIT OR Apache-2.0 |
| [dirs-sys](https://github.com/dirs-dev/dirs-sys-rs) | 0.4.1 | MIT OR Apache-2.0 |
| [dispatch2](https://github.com/madsmtm/objc2) | 0.3.1 | Zlib OR Apache-2.0 OR MIT |
| [displaydoc](https://github.com/yaahc/displaydoc) | 0.2.7 | MIT OR Apache-2.0 |
| [document-features](https://github.com/slint-ui/document-features) | 0.2.12 | MIT OR Apache-2.0 |
| [dpi](https://github.com/rust-windowing/winit) | 0.1.2 | Apache-2.0 AND MIT |
| [dyn-clone](https://github.com/dtolnay/dyn-clone) | 1.0.20 | MIT OR Apache-2.0 |
| [either](https://github.com/rayon-rs/either) | 1.18.0 | MIT OR Apache-2.0 |
| [env_logger](https://github.com/rust-cli/env_logger) | 0.10.2 | MIT OR Apache-2.0 |
| [equivalent](https://github.com/indexmap-rs/equivalent) | 1.0.2 | Apache-2.0 OR MIT |
| [errno](https://github.com/lambda-fairy/rust-errno) | 0.3.14 | MIT OR Apache-2.0 |
| [fallible-iterator](https://github.com/sfackler/rust-fallible-iterator) | 0.3.0 | MIT/Apache-2.0 |
| [fallible-streaming-iterator](https://github.com/sfackler/fallible-streaming-iterator) | 0.1.9 | MIT/Apache-2.0 |
| [fastrand](https://github.com/smol-rs/fastrand) | 2.5.0 | Apache-2.0 OR MIT |
| [fdeflate](https://github.com/image-rs/fdeflate) | 0.3.7 | MIT OR Apache-2.0 |
| [filetime](https://github.com/alexcrichton/filetime) | 0.2.29 | MIT/Apache-2.0 |
| [find-msvc-tools](https://github.com/rust-lang/cc-rs) | 0.1.13 | MIT OR Apache-2.0 |
| [flate2](https://github.com/rust-lang/flate2-rs) | 1.1.10 | MIT OR Apache-2.0 |
| [fnv](https://github.com/servo/rust-fnv) | 1.0.7 | Apache-2.0 / MIT |
| [foldhash](https://github.com/orlp/foldhash) | 0.2.0 | Zlib |
| [foreign-types](https://github.com/sfackler/foreign-types) | 0.5.0 | MIT/Apache-2.0 |
| [foreign-types-macros](https://github.com/sfackler/foreign-types) | 0.2.4 | MIT/Apache-2.0 |
| [foreign-types-shared](https://github.com/sfackler/foreign-types) | 0.3.1 | MIT/Apache-2.0 |
| [form_urlencoded](https://github.com/servo/rust-url) | 1.2.2 | MIT OR Apache-2.0 |
| [fs_extra](https://github.com/webdesus/fs_extra) | 1.3.0 | MIT |
| [futures](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [futures-channel](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [futures-core](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [futures-executor](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [futures-io](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [futures-macro](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [futures-sink](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [futures-task](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [futures-util](https://github.com/rust-lang/futures-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [generic-array](https://github.com/fizyk20/generic-array.git) | 0.14.7 | MIT |
| [getrandom](https://github.com/rust-random/getrandom) | 0.2.17 | MIT OR Apache-2.0 |
| [getrandom](https://github.com/rust-random/getrandom) | 0.3.4 | MIT OR Apache-2.0 |
| [getrandom](https://github.com/rust-random/getrandom) | 0.4.3 | MIT OR Apache-2.0 |
| [glob](https://github.com/rust-lang/glob) | 0.3.4 | MIT OR Apache-2.0 |
| [global-hotkey](https://github.com/tauri-apps/global-hotkey) | 0.8.0 | Apache-2.0 OR MIT |
| [hashbrown](https://github.com/rust-lang/hashbrown) | 0.17.1 | MIT OR Apache-2.0 |
| [hashlink](https://github.com/djc/hashlink) | 0.12.2 | MIT OR Apache-2.0 |
| [heck](https://github.com/withoutboats/heck) | 0.5.0 | MIT OR Apache-2.0 |
| [home](https://github.com/rust-lang/cargo) | 0.5.12 | MIT OR Apache-2.0 |
| [hound](https://github.com/ruuda/hound) | 3.5.1 | Apache-2.0 |
| [http](https://github.com/hyperium/http) | 1.5.0 | MIT OR Apache-2.0 |
| [httparse](https://github.com/seanmonstar/httparse) | 1.10.1 | MIT OR Apache-2.0 |
| [humantime](https://github.com/chronotope/humantime) | 2.4.0 | MIT OR Apache-2.0 |
| [iana-time-zone](https://github.com/strawlab/iana-time-zone) | 0.1.65 | MIT OR Apache-2.0 |
| [icu_collections](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 |
| [icu_locale_core](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 |
| [icu_normalizer](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 |
| [icu_normalizer_data](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 |
| [icu_properties](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 |
| [icu_properties_data](https://github.com/unicode-org/icu4x) | 2.3.0 | Unicode-3.0 |
| [icu_provider](https://github.com/unicode-org/icu4x) | 2.3.1 | Unicode-3.0 |
| [ident_case](https://github.com/TedDriggs/ident_case) | 1.0.1 | MIT/Apache-2.0 |
| [idna](https://github.com/servo/rust-url/) | 1.1.0 | MIT OR Apache-2.0 |
| [idna_adapter](https://github.com/hsivonen/idna_adapter) | 1.2.2 | Apache-2.0 OR MIT |
| [indexmap](https://github.com/indexmap-rs/indexmap) | 2.14.2 | Apache-2.0 OR MIT |
| [is-terminal](https://github.com/sunfishcode/is-terminal) | 0.4.17 | MIT |
| [is_terminal_polyfill](https://github.com/polyfill-rs/is_terminal_polyfill) | 1.70.2 | MIT OR Apache-2.0 |
| [itertools](https://github.com/rust-itertools/itertools) | 0.12.1 | MIT OR Apache-2.0 |
| [itoa](https://github.com/dtolnay/itoa) | 1.0.18 | MIT OR Apache-2.0 |
| [keyboard-types](https://github.com/pyfisch/keyboard-types) | 0.7.0 | MIT OR Apache-2.0 |
| [lazy_static](https://github.com/rust-lang-nursery/lazy-static.rs) | 1.5.0 | MIT OR Apache-2.0 |
| [lazycell](https://github.com/indiv0/lazycell) | 1.3.0 | MIT/Apache-2.0 |
| [libc](https://github.com/rust-lang/libc) | 0.2.189 | MIT OR Apache-2.0 |
| [libloading](https://github.com/nagisa/rust_libloading/) | 0.8.9 | ISC |
| [libsqlite3-sys](https://github.com/rusqlite/rusqlite) | 0.38.2 | MIT |
| [litemap](https://github.com/unicode-org/icu4x) | 0.8.3 | Unicode-3.0 |
| [litrs](https://github.com/LukasKalbertodt/litrs) | 1.0.0 | MIT OR Apache-2.0 |
| [log](https://github.com/rust-lang/log) | 0.4.34 | MIT OR Apache-2.0 |
| [mac-notification-sys](https://github.com/h4llow3En/mac-notification-sys) | 0.6.15 | MIT/Apache-2.0 |
| [mach2](https://github.com/JohnTitor/mach2) | 0.6.0 | BSD-2-Clause OR MIT OR Apache-2.0 |
| [matchers](https://github.com/hawkw/matchers) | 0.2.0 | MIT |
| [matrixmultiply](https://github.com/bluss/matrixmultiply/) | 0.3.11 | MIT/Apache-2.0 |
| [memchr](https://github.com/BurntSushi/memchr) | 2.8.3 | Unlicense OR MIT |
| [minimal-lexical](https://github.com/Alexhuszagh/minimal-lexical) | 0.2.1 | MIT/Apache-2.0 |
| [miniz_oxide](https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide) | 0.8.9 | MIT OR Zlib OR Apache-2.0 |
| [miniz_oxide](https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide) | 0.9.1 | MIT OR Zlib OR Apache-2.0 |
| [mio](https://github.com/tokio-rs/mio) | 1.2.3 | MIT |
| [muda](https://github.com/tauri-apps/muda) | 0.17.2 | Apache-2.0 OR MIT |
| [native-tls](https://github.com/rust-native-tls/rust-native-tls) | 0.2.18 | MIT OR Apache-2.0 |
| [ndarray](https://github.com/rust-ndarray/ndarray) | 0.16.1 | MIT OR Apache-2.0 |
| [nom](https://github.com/Geal/nom) | 7.1.3 | MIT |
| [notify-rust](https://github.com/hoodie/notify-rust) | 4.18.0 | MIT OR Apache-2.0 |
| [nu-ansi-term](https://github.com/nushell/nu-ansi-term) | 0.50.3 | MIT |
| [num-complex](https://github.com/rust-num/num-complex) | 0.4.6 | MIT OR Apache-2.0 |
| [num-conv](https://github.com/jhpratt/num-conv) | 0.2.2 | MIT OR Apache-2.0 |
| [num-integer](https://github.com/rust-num/num-integer) | 0.1.47 | MIT OR Apache-2.0 |
| [num-traits](https://github.com/rust-num/num-traits) | 0.2.19 | MIT OR Apache-2.0 |
| [objc2](https://github.com/madsmtm/objc2) | 0.6.4 | MIT |
| [objc2-app-kit](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-application-services](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-audio-toolbox](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-cloud-kit](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-core-audio](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-core-audio-types](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-core-data](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-core-foundation](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-core-graphics](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-core-image](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-core-services](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-core-text](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-core-video](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-encode](https://github.com/madsmtm/objc2) | 4.1.0 | MIT |
| [objc2-foundation](https://github.com/madsmtm/objc2) | 0.3.2 | MIT |
| [objc2-io-surface](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-metal](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-quartz-core](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [objc2-security](https://github.com/madsmtm/objc2) | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| [once_cell](https://github.com/matklad/once_cell) | 1.21.4 | MIT OR Apache-2.0 |
| [option-ext](https://github.com/soc/option-ext.git) | 0.2.0 | MPL-2.0 |
| [ort](https://github.com/pykeio/ort) | 2.0.0-rc.10 | MIT OR Apache-2.0 |
| [ort-sys](https://github.com/pykeio/ort) | 2.0.0-rc.10 | MIT OR Apache-2.0 |
| [pastey](https://github.com/as1100k/pastey) | 0.2.3 | MIT OR Apache-2.0 |
| [pem-rfc7468](https://github.com/RustCrypto/formats) | 1.0.0 | Apache-2.0 OR MIT |
| [percent-encoding](https://github.com/servo/rust-url/) | 2.3.2 | MIT OR Apache-2.0 |
| [pin-project-lite](https://github.com/taiki-e/pin-project-lite) | 0.2.17 | Apache-2.0 OR MIT |
| [pkg-config](https://github.com/rust-lang/pkg-config-rs) | 0.3.34 | MIT OR Apache-2.0 |
| [png](https://github.com/image-rs/image-png) | 0.17.16 | MIT OR Apache-2.0 |
| [potential_utf](https://github.com/unicode-org/icu4x) | 0.1.6 | Unicode-3.0 |
| [powerfmt](https://github.com/jhpratt/powerfmt) | 0.2.0 | MIT OR Apache-2.0 |
| [ppv-lite86](https://github.com/cryptocorrosion/cryptocorrosion) | 0.2.21 | MIT OR Apache-2.0 |
| [prettyplease](https://github.com/dtolnay/prettyplease) | 0.2.37 | MIT OR Apache-2.0 |
| [primal-check](https://github.com/huonw/primal) | 0.3.4 | MIT OR Apache-2.0 |
| [proc-macro2](https://github.com/dtolnay/proc-macro2) | 1.0.107 | MIT OR Apache-2.0 |
| [proptest](https://github.com/proptest-rs/proptest) | 1.11.0 | MIT OR Apache-2.0 |
| [quick-error](http://github.com/tailhook/quick-error) | 1.2.3 | MIT/Apache-2.0 |
| [quote](https://github.com/dtolnay/quote) | 1.0.47 | MIT OR Apache-2.0 |
| [rand](https://github.com/rust-random/rand) | 0.9.5 | MIT OR Apache-2.0 |
| [rand_chacha](https://github.com/rust-random/rand) | 0.9.0 | MIT OR Apache-2.0 |
| [rand_core](https://github.com/rust-random/rand) | 0.9.5 | MIT OR Apache-2.0 |
| [rand_xorshift](https://github.com/rust-random/rngs) | 0.4.0 | MIT OR Apache-2.0 |
| [raw-window-handle](https://github.com/rust-windowing/raw-window-handle) | 0.6.2 | MIT OR Apache-2.0 OR Zlib |
| [rawpointer](https://github.com/bluss/rawpointer/) | 0.2.1 | MIT/Apache-2.0 |
| [realfft](https://github.com/HEnquist/realfft) | 3.5.0 | MIT |
| [ref-cast](https://github.com/dtolnay/ref-cast) | 1.0.27 | MIT OR Apache-2.0 |
| [ref-cast-impl](https://github.com/dtolnay/ref-cast) | 1.0.27 | MIT OR Apache-2.0 |
| [regex](https://github.com/rust-lang/regex) | 1.13.1 | MIT OR Apache-2.0 |
| [regex-automata](https://github.com/rust-lang/regex) | 0.4.18 | MIT OR Apache-2.0 |
| [regex-syntax](https://github.com/rust-lang/regex) | 0.8.11 | MIT OR Apache-2.0 |
| [ring](https://github.com/briansmith/ring) | 0.17.14 | Apache-2.0 AND ISC |
| [rmcp](https://github.com/modelcontextprotocol/rust-sdk/) | 3.4.0 | Apache-2.0 |
| [rmcp-macros](https://github.com/modelcontextprotocol/rust-sdk/) | 3.4.0 | Apache-2.0 |
| [rubato](https://github.com/HEnquist/rubato) | 5.0.0 | MIT OR Apache-2.0 |
| [rusqlite](https://github.com/rusqlite/rusqlite) | 0.40.2 | MIT |
| [rustc-hash](https://github.com/rust-lang-nursery/rustc-hash) | 1.1.0 | Apache-2.0/MIT |
| [rustfft](https://github.com/ejmahler/RustFFT) | 6.4.1 | MIT OR Apache-2.0 |
| [rustix](https://github.com/bytecodealliance/rustix) | 0.38.44 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [rustix](https://github.com/bytecodealliance/rustix) | 1.1.5 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| [rustls](https://github.com/rustls/rustls) | 0.23.45 | Apache-2.0 OR ISC OR MIT |
| [rustls-pki-types](https://github.com/rustls/pki-types) | 1.15.1 | MIT OR Apache-2.0 |
| [rustls-webpki](https://github.com/rustls/webpki) | 0.103.15 | ISC |
| [rusty-fork](https://github.com/altsysrq/rusty-fork) | 0.3.1 | MIT/Apache-2.0 |
| [schemars](https://github.com/GREsau/schemars) | 1.2.2 | MIT |
| [schemars_derive](https://github.com/GREsau/schemars) | 1.2.2 | MIT |
| [security-framework](https://github.com/kornelski/rust-security-framework) | 3.7.0 | MIT OR Apache-2.0 |
| [security-framework-sys](https://github.com/kornelski/rust-security-framework) | 2.17.0 | MIT OR Apache-2.0 |
| [serde](https://github.com/serde-rs/serde) | 1.0.229 | MIT OR Apache-2.0 |
| [serde_core](https://github.com/serde-rs/serde) | 1.0.229 | MIT OR Apache-2.0 |
| [serde_derive](https://github.com/serde-rs/serde) | 1.0.229 | MIT OR Apache-2.0 |
| [serde_derive_internals](https://github.com/serde-rs/serde) | 0.30.0 | MIT OR Apache-2.0 |
| [serde_json](https://github.com/serde-rs/json) | 1.0.151 | MIT OR Apache-2.0 |
| [serde_spanned](https://github.com/toml-rs/toml) | 0.6.9 | MIT OR Apache-2.0 |
| [sha2](https://github.com/RustCrypto/hashes) | 0.10.9 | MIT OR Apache-2.0 |
| [sharded-slab](https://github.com/hawkw/sharded-slab) | 0.1.7 | MIT |
| [shlex](https://github.com/comex/rust-shlex) | 1.3.0 | MIT OR Apache-2.0 |
| [shlex](https://github.com/comex/rust-shlex) | 2.0.1 | MIT OR Apache-2.0 |
| [signal-hook-registry](https://github.com/vorner/signal-hook) | 1.4.8 | MIT OR Apache-2.0 |
| [simd-adler32](https://github.com/mcountryman/simd-adler32) | 0.3.10 | MIT |
| [slab](https://github.com/tokio-rs/slab) | 0.4.12 | MIT |
| [smallvec](https://github.com/servo/rust-smallvec) | 1.16.1 | MIT OR Apache-2.0 |
| [smallvec](https://github.com/servo/rust-smallvec) | 2.0.0-alpha.10 | MIT OR Apache-2.0 |
| [socket2](https://github.com/rust-lang/socket2) | 0.6.5 | MIT OR Apache-2.0 |
| [socks](https://github.com/sfackler/rust-socks) | 0.3.4 | MIT/Apache-2.0 |
| [stable_deref_trait](https://github.com/storyyeller/stable_deref_trait) | 1.2.1 | MIT OR Apache-2.0 |
| [strength_reduce](http://github.com/ejmahler/strength_reduce) | 0.2.4 | MIT OR Apache-2.0 |
| [strsim](https://github.com/rapidfuzz/strsim-rs) | 0.11.1 | MIT |
| [subtle](https://github.com/dalek-cryptography/subtle) | 2.6.1 | BSD-3-Clause |
| [symlink](https://gitlab.com/chris-morgan/symlink) | 0.1.0 | MIT/Apache-2.0 |
| [syn](https://github.com/dtolnay/syn) | 2.0.119 | MIT OR Apache-2.0 |
| [syn](https://github.com/dtolnay/syn) | 3.0.6 | MIT OR Apache-2.0 |
| [synstructure](https://github.com/mystor/synstructure) | 0.14.0 | MIT |
| [tao](https://github.com/tauri-apps/tao) | 0.35.3 | Apache-2.0 |
| [tar](https://github.com/composefs/tar-rs) | 0.4.46 | MIT OR Apache-2.0 |
| [tempfile](https://github.com/Stebalien/tempfile) | 3.27.0 | MIT OR Apache-2.0 |
| [termcolor](https://github.com/BurntSushi/termcolor) | 1.4.1 | Unlicense OR MIT |
| [thiserror](https://github.com/dtolnay/thiserror) | 2.0.20 | MIT OR Apache-2.0 |
| [thiserror-impl](https://github.com/dtolnay/thiserror) | 2.0.20 | MIT OR Apache-2.0 |
| [thread_local](https://github.com/Amanieu/thread_local-rs) | 1.1.10 | MIT OR Apache-2.0 |
| [time](https://github.com/time-rs/time) | 0.3.55 | MIT OR Apache-2.0 |
| [time-core](https://github.com/time-rs/time) | 0.1.9 | MIT OR Apache-2.0 |
| [time-macros](https://github.com/time-rs/time) | 0.2.32 | MIT OR Apache-2.0 |
| [tinystr](https://github.com/unicode-org/icu4x) | 0.8.4 | Unicode-3.0 |
| [tinyvec](https://github.com/Lokathor/tinyvec) | 1.13.3 | Zlib OR Apache-2.0 OR MIT |
| [tokio](https://github.com/tokio-rs/tokio) | 1.53.1 | MIT |
| [tokio-macros](https://github.com/tokio-rs/tokio) | 2.7.2 | MIT |
| [tokio-util](https://github.com/tokio-rs/tokio) | 0.7.19 | MIT |
| [toml](https://github.com/toml-rs/toml) | 0.8.2 | MIT OR Apache-2.0 |
| [toml_datetime](https://github.com/toml-rs/toml) | 0.6.3 | MIT OR Apache-2.0 |
| [toml_edit](https://github.com/toml-rs/toml) | 0.20.2 | MIT OR Apache-2.0 |
| [tracing](https://github.com/tokio-rs/tracing) | 0.1.44 | MIT |
| [tracing-appender](https://github.com/tokio-rs/tracing) | 0.2.5 | MIT |
| [tracing-attributes](https://github.com/tokio-rs/tracing) | 0.1.31 | MIT |
| [tracing-core](https://github.com/tokio-rs/tracing) | 0.1.36 | MIT |
| [tracing-log](https://github.com/tokio-rs/tracing) | 0.2.0 | MIT |
| [tracing-serde](https://github.com/tokio-rs/tracing) | 0.2.0 | MIT |
| [tracing-subscriber](https://github.com/tokio-rs/tracing) | 0.3.23 | MIT |
| [transcribe-rs](https://github.com/cjpais/transcribe-rs) | 0.3.1 | MIT |
| [transpose](https://github.com/ejmahler/transpose) | 0.2.3 | MIT OR Apache-2.0 |
| [tray-icon](https://github.com/tauri-apps/tray-icon) | 0.21.3 | MIT OR Apache-2.0 |
| [typenum](https://github.com/paholg/typenum) | 1.20.1 | MIT OR Apache-2.0 |
| [unarray](https://github.com/cameron1024/unarray) | 0.1.4 | MIT OR Apache-2.0 |
| [unicode-ident](https://github.com/dtolnay/unicode-ident) | 1.0.26 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| [unicode-normalization](https://github.com/unicode-rs/unicode-normalization) | 0.1.25 | MIT OR Apache-2.0 |
| [unicode-segmentation](https://github.com/unicode-rs/unicode-segmentation) | 1.13.3 | MIT OR Apache-2.0 |
| [untrusted](https://github.com/briansmith/untrusted) | 0.9.0 | ISC |
| [ureq](https://github.com/algesten/ureq) | 3.4.2 | MIT OR Apache-2.0 |
| [ureq-proto](https://github.com/algesten/ureq-proto) | 0.6.4 | MIT OR Apache-2.0 |
| [url](https://github.com/servo/rust-url) | 2.5.8 | MIT OR Apache-2.0 |
| [utf8-zero](https://github.com/algesten/utf8-zero) | 0.8.1 | MIT OR Apache-2.0 |
| [utf8_iter](https://github.com/hsivonen/utf8_iter) | 1.0.4 | Apache-2.0 OR MIT |
| [utf8parse](https://github.com/alacritty/vte) | 0.2.2 | Apache-2.0 OR MIT |
| [uuid](https://github.com/uuid-rs/uuid) | 1.26.1 | Apache-2.0 OR MIT |
| [vcpkg](https://github.com/mcgoo/vcpkg-rs) | 0.2.15 | MIT/Apache-2.0 |
| [version_check](https://github.com/SergioBenitez/version_check) | 0.9.5 | MIT/Apache-2.0 |
| [visibility](https://github.com/danielhenrymantilla/visibility.rs) | 0.1.1 | Zlib OR MIT OR Apache-2.0 |
| [wait-timeout](https://github.com/alexcrichton/wait-timeout) | 0.2.1 | MIT/Apache-2.0 |
| [webpki-root-certs](https://github.com/rustls/webpki-roots) | 1.0.9 | CDLA-Permissive-2.0 |
| [webpki-roots](https://github.com/rustls/webpki-roots) | 1.0.9 | CDLA-Permissive-2.0 |
| [which](https://github.com/harryfei/which-rs.git) | 4.4.2 | MIT |
| [whisper-rs](https://github.com/tazz4843/whisper-rs) | 0.13.2 | Unlicense |
| [whisper-rs-sys](https://github.com/tazz4843/whisper-rs) | 0.11.1 | Unlicense |
| [windowfunctions](https://github.com/HEnquist/windowfunctions-rs) | 0.1.1 | MIT |
| [winnow](https://github.com/winnow-rs/winnow) | 0.5.40 | MIT |
| [writeable](https://github.com/unicode-org/icu4x) | 0.6.4 | Unicode-3.0 |
| [xattr](https://github.com/Stebalien/xattr) | 1.6.1 | MIT OR Apache-2.0 |
| [yoke](https://github.com/unicode-org/icu4x) | 0.8.3 | Unicode-3.0 |
| [yoke-derive](https://github.com/unicode-org/icu4x) | 0.8.3 | Unicode-3.0 |
| [zerocopy](https://github.com/google/zerocopy) | 0.8.57 | BSD-2-Clause OR Apache-2.0 OR MIT |
| [zerofrom](https://github.com/unicode-org/icu4x) | 0.1.8 | Unicode-3.0 |
| [zerofrom-derive](https://github.com/unicode-org/icu4x) | 0.1.8 | Unicode-3.0 |
| [zeroize](https://github.com/RustCrypto/utils) | 1.9.0 | Apache-2.0 OR MIT |
| [zerotrie](https://github.com/unicode-org/icu4x) | 0.2.5 | Unicode-3.0 |
| [zerovec](https://github.com/unicode-org/icu4x) | 0.11.8 | Unicode-3.0 |
| [zerovec-derive](https://github.com/unicode-org/icu4x) | 0.11.6 | Unicode-3.0 |
| [zlib-rs](https://github.com/trifectatechfoundation/zlib-rs) | 0.6.8 | Zlib |
| [zmij](https://github.com/dtolnay/zmij) | 1.0.23 | MIT |
