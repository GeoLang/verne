# Contributing to Geodukt

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone git@github.com:YOUR_USER/geodukt.git`
3. Create a feature branch: `git checkout -b my-feature`
4. Make changes and add tests
5. Run checks: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all`
6. Commit and push
7. Open a pull request

## Code Style

- Run `cargo fmt --all` before committing
- All clippy warnings must be resolved
- Add tests for new functionality

## Adding Transforms

Implement the `TransformOp` trait from `geodukt-core` and register in `geodukt-transforms/src/registry.rs`.

## Adding I/O Formats

Implement `SourceReader` and/or `SinkWriter` from `geodukt-core::pipeline`.
