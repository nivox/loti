//! Black-box conformance suite for the `loti` binary.
//!
//! Each submodule groups checks for one area of the normative behaviour the CLI
//! must exhibit, driving the compiled binary as a subprocess against throwaway
//! stores. The module names use a `spec_NN_*` taxonomy purely as an internal
//! index into the specification's sections; no such marker ever appears in what
//! the binary prints.
//!
//! This suite is the living record that the tool reaches its destination: the
//! full CLI surface behaves as specified across the domain/state machine,
//! on-disk storage & discovery, CLI grammar, read/output formats, filtering,
//! the skill/help surface, concurrency, and format versioning.

mod conformance {
    pub mod harness;

    mod spec_02_domain;
    mod spec_03_storage;
    mod spec_04_grammar;
    mod spec_05_read_output;
    mod spec_06_filtering;
    mod spec_07_skill_help;
    mod spec_08_concurrency;
    mod spec_09_versioning;
}
