# Chaos Kernel Debugging & Rewriting Homework

## IMPORTANT NOTICE -- READ BEFORE YOU START

**Direct use of LLMs (Large Language Models) to generate your solution is strictly prohibited.**

You are allowed and encouraged to use AI agents (e.g., Cursor Agent, Copilot Chat, etc.) as *tools* during your workflow. However, all submissions must include:

1. **Your complete agent dialogue logs.** Export and attach the full conversation history with any AI agent you used. **Only inline TAB operation can be waived.**
2. **A clear annotation** in your submission indicating which parts of the code were **written by you (human)** and which parts were **generated or suggested by the agent**. You may annotate inline with comments like `// HUMAN` and `// AGENT`, or provide a separate summary document.

**Any violation of this policy -- including but not limited to submitting agent-generated code without disclosure, fabricating dialogue logs, or using an LLM to produce the final solution directly -- will result in a score of zero (0) for the entire assignment.**

The purpose of this policy is to ensure you *understand* the bugs and the code you are fixing. The agent is a collaborator, not a substitute for your own reasoning.

---

## Overview

This project is built on top of the [rCore](https://github.com/rcore-os/rCore) teaching operating system. The active host-side simulator now lives in `kernel/` (Cargo package name `kernel-sim`). The old monolithic rCore file is preserved under `kernel-legacy/src/kernel.rs`.

A comprehensive test suite is provided under `chaos-tests/`.

## Your Tasks

### Task 1: Debug and Pass All Tests

The active kernel simulator code in `kernel/` is the current debugging and rewriting target.

Your goal is to **find and fix all bugs** so that the entire test suite passes:

```bash
# Run the basic tests
cargo test --test basic

# Run the advanced tests
cargo test --test advanced

# Run the pressure tests
cargo test --test pressure
```

All three test groups must pass for full credit.

> **Clarifications (April 28)**
>
> `chaos-tests` now depends on the `kernel-sim` Cargo package through `path = "../kernel"`.
> Therefore, changes made under `kernel/` are reflected in the test suites.

### Task 2: Rewrite the Code

After debugging, **rewrite the relevant code under `kernel/`** to improve code quality:

- Add clear, meaningful comments where appropriate
- Rename cryptic variables and functions to descriptive names
- Extract inlined logic into well-named helper functions
- Simplify unnecessarily complex expressions
- Ensure the rewritten code still passes all tests

The rewrite is graded on **readability, structure, and correctness**.

## Project Structure

```
chaos/
├── kernel/                 # Active host userspace simulator
├── kernel-legacy/          # Old rCore source tree, normally not edited
├── kernel-qemu/            # RISC-V QEMU no_std carrier and migration target
├── chaos-tests/            # Test suite
└── README.md               # This file
```

## Getting Started

1. Try to compile the project first -- fix all compilation errors:
   ```bash
   cargo check
   ```

2. Once it compiles, run the basic tests and fix bugs one group at a time:
   ```bash
   cargo test --test basic -- group_01
   ```

3. Move on to advanced and pressure tests after basic tests pass.

4. Rewrite the code for clarity after all tests pass.

## Grading

| Component | Weight |
|-----------|--------|
| Basic tests passing | 20% |
| Advanced tests passing | 20% |
| Pressure tests passing | 20% |
| Code Review | 40% |
