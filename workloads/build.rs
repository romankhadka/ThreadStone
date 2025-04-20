fn main() {
    cc::Build::new()
        .include("native/dhry")

        // Fixes already added last round
        .flag_if_supported("-std=c99")
        // ── 1.  use the dialect Dhrystone was written for ────────────────
        .flag_if_supported("-std=gnu89")          // allow implicit‑int + K&R
        .flag_if_supported("-Wno-implicit-int")   // silence the old style
        .flag_if_supported("-Wno-implicit-function-declaration")
        .flag_if_supported("-Wno-deprecated-non-prototype")
        .flag_if_supported("-Wno-incompatible-library-redeclaration")

        // ── silence the built‑in main & timer baggage ────────────────
        .define("main", "dhry_unused_main")   // avoids duplicate main symbol
         // avoids duplicate Proc_0 symbol
        .define("TIMES", "1")                 // use <sys/times.h>, no custom decl
        .define("HZ", "60")                   // stops the undeclared‑HZ errors
        .define("NO_IO", "1")                 // printf/scanf no longer needed
        .define("DHRYSTONE_NO_MAIN", "1")     // bypasses the benchmark's own timer
        .define("DHRY_ITERS", "1000000")
        .define("QUIET", "1")                 // suppress all diagnostic output

        // ── Automatically pull in <string.h> so strcpy/strcmp are declared ─
        .flag_if_supported("-include")
        .flag_if_supported("string.h")
        .flag_if_supported("-Wno-builtin-requires-header")

        // ── Compile units ─────────────────────────────────────────────────
        .files([
            "native/dhry/dhry_1.c",
            "native/dhry/dhry_2.c",
            "native/dhry/wrapper.c",
            "native/dhry/shim.c",
        ])
        .flag_if_supported("-O3")
        .compile("libdhry.a");
}
