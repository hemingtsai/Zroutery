fn main() {
    // A sandbox cannot search the host's VS/PATH for VCRUNTIME140.dll. Keep
    // the trusted worker self-contained without granting that directory read.
    static_vcruntime::metabuild();
}
