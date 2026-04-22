# Contributing to lofs

Thanks for your interest in lofs!

> **Status note:** lofs is currently a concept / infrastructure scaffold. The design is still under review. Code contributions are welcome once ADR-001 is accepted; until then the most valuable contributions are **discussion on design docs** and **review of research assumptions**.

## Ways to contribute

1. **Read [ADR-001](docs/architecture/adr/ADR-001-lofs.md) and file issues on gaps / questions / disagreements.**
2. **Review the research documents** ([docs/architecture/research/](docs/architecture/research/)) — call out findings we missed or citations that are wrong.
3. Propose additional use-cases or counter-examples in GitHub Discussions.
4. Report similar OSS projects we missed — the "gap analysis" in RESEARCH-006 is an ongoing effort.

## Prerequisites (for code contributions, once unblocked)

- **Rust** 1.85+ (edition 2024)
- Linux kernel 5.11+ (user namespaces + unprivileged overlayfs)
- Optional: `fuse-overlayfs`, `buildah` for fallback paths
- Docker-compatible OCI registry for integration tests (Zot, Harbor)

macOS: `brew install rust` + development will run through a Linux VM (Colima / Lima) since rootless overlayfs is Linux-only.

## Development workflow (future)

```bash
git clone https://github.com/meteora-pro/lofs.git
cd lofs
cargo build --workspace
cargo test --workspace
```

Standard checks before a PR:

```bash
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

## Conventional Commits

```
feat(scope): description (#issue)
fix(scope): description (#issue)
docs: description
test: description
chore: description
```

## Branch naming

- `feat/<issue>-<slug>`
- `fix/<issue>-<slug>`
- `docs/<slug>`

## PR checklist

- [ ] `cargo fmt --all --check` clean
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes
- [ ] Commit message follows Conventional Commits
- [ ] PR description explains what and why
- [ ] If design-affecting: updated ADR referenced and changelog updated

## Code style

- **Rust:** `cargo fmt` + `cargo clippy`
- **Code:** variable / function / type names in English
- **Comments:** English (this is an international OSS project)
- **Docs:** primary English (`README.md`), Russian mirror (`README.ru.md`) where applicable

## Security

If you find a security issue, email `ai-dev@meteora.pro` privately. Do not open a public GitHub issue for vulnerabilities.

## License

By contributing you agree that your contributions are licensed under the [MIT License](LICENSE).
