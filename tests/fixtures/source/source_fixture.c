// Copyright 2026 Netflix, Inc.
// SPDX-License-Identifier: Apache-2.0

// A fixed FreeBSD ELF/DWARF input for source lookup on every host OS.
__attribute__((noinline)) unsigned long event_explorer_source_fixture(void) {
    volatile unsigned long value = 42;
    return value;
}
