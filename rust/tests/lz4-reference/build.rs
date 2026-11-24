//! Builds each reference liblz4 version into its own static lib with its
//! global symbols hidden (`LZ4LIB_VISIBILITY=static`) or renamed per
//! version, so several versions can be linked into one test binary.
//! Test/bench only; never shipped.

/// Globals `LZ4LIB_VISIBILITY=static` does not cover in 1.9.1 and 1.10.0.
const LEFTOVER_GLOBALS: &[&str] = &[
  "LZ4_compress_HC_continue_destSize",
  "LZ4_compress_HC_destSize",
  "LZ4_compress_HC_extStateHC_fastReset",
  "LZ4_favorDecompressionSpeed",
  "LZ4_resetStreamHC_fast",
  "LZ4_setCompressionLevel",
  "LZ4_attach_HC_dictionary",
  "LZ4HC_searchExtDict",
];

/// r131 predates `LZ4LIB_VISIBILITY`; every global of its `lz4hc.c`.
const R131_GLOBALS: &[&str] = &[
  "LZ4_compressHC",
  "LZ4_compressHC2",
  "LZ4_compressHC2_continue",
  "LZ4_compressHC2_limitedOutput",
  "LZ4_compressHC2_limitedOutput_continue",
  "LZ4_compressHC2_limitedOutput_withStateHC",
  "LZ4_compressHC2_withStateHC",
  "LZ4_compressHC_continue",
  "LZ4_compressHC_limitedOutput",
  "LZ4_compressHC_limitedOutput_continue",
  "LZ4_compressHC_limitedOutput_withStateHC",
  "LZ4_compressHC_withStateHC",
  "LZ4_compress_HC",
  "LZ4_compress_HC_continue",
  "LZ4_compress_HC_extStateHC",
  "LZ4_createHC",
  "LZ4_createStreamHC",
  "LZ4_freeHC",
  "LZ4_freeStreamHC",
  "LZ4_loadDictHC",
  "LZ4_resetStreamHC",
  "LZ4_resetStreamStateHC",
  "LZ4_saveDictHC",
  "LZ4_sizeofStateHC",
  "LZ4_sizeofStreamStateHC",
  "LZ4_slideInputBufferHC",
];

/// Vendor directory, symbol prefix, globals to rename.
const VERSIONS: &[(&str, &str, &[&str])] = &[
  ("r131", "r131", R131_GLOBALS),
  ("1.9.1", "v1_9_1", LEFTOVER_GLOBALS),
  ("1.10.0", "v1_10_0", LEFTOVER_GLOBALS),
];

/// The `lz4hc.c` of r131 and 1.9.1 calls `LZ4_compressBound` without
/// defining it; one shared definition serves all of them.
const COMPRESS_BOUND: &str = "int LZ4_compressBound(int n) { return LZ4_COMPRESSBOUND(n); }\n";

fn main() {
  let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
  let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
  for (i, &(dir, tag, globals)) in VERSIONS.iter().enumerate() {
    let lib = manifest.join("vendor").join(dir).join("lib");
    let shim = out.join(format!("{tag}.c"));
    std::fs::write(
      &shim,
      format!(
        "#include \"lz4hc.c\"\n{}int {tag}_compress_bound(int n) {{ return LZ4_COMPRESSBOUND(n); }}\nint {tag}_compress_hc(const char* s, \
         char* d, int n, int cap, int lvl) {{ return LZ4_compress_HC(s, d, n, cap, lvl); }}\nint {tag}_version_number(void) {{ return \
         LZ4_VERSION_NUMBER; }}\n",
        if i == 0 { COMPRESS_BOUND } else { "" }
      ),
    )
    .unwrap();
    let mut build = cc::Build::new();
    build
      .file(&shim)
      .include(&lib)
      .opt_level(3)
      .define("LZ4LIB_VISIBILITY", "static")
      .warnings(false)
      .flag_if_supported("-w");
    for symbol in globals {
      build.define(symbol, Some(format!("{tag}_{symbol}").as_str()));
    }
    build.compile(tag);
    println!("cargo:rerun-if-changed={}", lib.join("lz4hc.c").display());
    println!("cargo:rerun-if-changed={}", lib.join("lz4.c").display());
  }
}
