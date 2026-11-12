# Source lookup fixture

`source_fixture.elf` is an x86_64 FreeBSD ELF containing DWARF for
`source_fixture.c`. It is parsed, never executed. A fixed ELF exercises
FreeBSD source lookup on Linux, FreeBSD, and macOS without relying on the
host's executable format or debug-info placement (Mach-O uses a dSYM).

Regenerate from the repository root with Clang and LLVM's linker (the Rust
toolchain ships `rust-lld` in `lib/rustlib/<host>/bin/`):

```sh
clang --target=x86_64-unknown-freebsd -gdwarf-4 -O0 -ffreestanding \
  -fdebug-prefix-map="$PWD"=/eventexplorer-test \
  -c tests/fixtures/source/source_fixture.c -o /tmp/ee-source-fixture.o
rust-lld -flavor gnu --build-id -e event_explorer_source_fixture \
  /tmp/ee-source-fixture.o -o tests/fixtures/source/source_fixture.elf
```

The test rewrites `/eventexplorer-test` to its checkout root.
