// build.rs — ta-workspace build script.
//
// On Windows MSVC, delay-load ProjectedFSLib.dll so the binary starts
// successfully on machines where the Windows "Client-ProjFS" optional feature
// is not installed. Without delay-loading, the OS loader rejects the binary
// at startup because the DLL is absent from System32 — even though TA falls
// back to a non-ProjFS strategy at runtime.
//
// With delay-loading:
//   1. Binary starts on all Windows machines.
//   2. is_projfs_available() probes for the DLL at runtime.
//   3. If unavailable, TA uses the copy-based workspace strategy.
//   4. PrjStartVirtualizing / PrjStopVirtualizing are never called, so the
//      delay-load resolver never needs to touch ProjectedFSLib.dll.

fn main() {
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    {
        // Delay-load ProjectedFSLib.dll — resolves symbols lazily at first call,
        // not at process startup. Requires delayimp.lib (ships with MSVC).
        println!("cargo:rustc-link-arg=/DELAYLOAD:ProjectedFSLib.dll");
        println!("cargo:rustc-link-lib=delayimp");
    }
}
