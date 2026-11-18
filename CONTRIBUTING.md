# Contributing to The SpongeBob SquarePants Movie - Nintendo Switch Port

Thanks for your interest in contributing to this project. We welcome bug fixes, optimization work, tooling improvements, compatibility fixes, and documentation updates.

## Ways to contribute

- Report bugs or build problems
- Improve performance or stability
- Fix compatibility issues
- Add or improve documentation
- Refine build scripts and tooling
- Help with code cleanup or refactoring when it remains focused and safe

## Community expectations

This project is a public, community-driven reverse-engineering and recompilation effort. Please keep discussions constructive and respectful.

- Be clear and specific in issue reports
- Keep pull requests focused on a single change
- Explain the reason for your change and any trade-offs
- Respect the project's legal and licensing boundaries

## Before you start

Please make sure you understand the project scope and constraints:

- This project is a native Nintendo Switch port/recompilation of a GameCube title.
- It relies on custom build tooling, Rust, C/C++, and devkitA64-related setup.
- Do not distribute proprietary game assets, copyrighted ROM content, or other restricted material in this repository.

## Development setup

### Prerequisites

- Git
- Rust toolchain
- Cargo
- Docker or Podman
- A working Linux environment for the provided build flow

### Local setup

1. Fork the repository
2. Clone your fork locally
3. Create a feature branch:

```bash
git checkout -b my-feature
```

4. Make your changes in a focused and readable way
5. Run the project build or relevant validation steps
6. Commit with a clear message
7. Open a pull request against the main branch

## Build and validation

Use the project’s existing build flow as the first validation step:

```bash
chmod +x build_switch.sh
./build_switch.sh
```

If your change affects a specific subcrate, platform target, or tooling path, validate that area as well. The most important requirement is that the change remains consistent with the project’s architecture and build flow.

## Pull request guidelines

- Keep PRs narrowly scoped and easy to review
- Include a clear summary of what changed and why
- Mention any testing or build steps you ran
- Avoid unrelated refactors or formatting-only changes in the same PR
- Update documentation when behavior, setup, or developer workflow changes

## Issue reporting

When creating an issue, please include:

- What you expected to happen
- What actually happened
- Relevant logs, commands, or build output
- Your environment details (OS, toolchain version, Docker/Podman status)
- Any reproduction steps if applicable

## Code style

- Follow the existing Rust, C, and build-script style in the repository
- Prefer small, readable, well-commented changes
- Keep naming consistent with the surrounding code
- Avoid broad rewrites unless clearly justified

## Legal and project boundaries

This project includes reverse-engineering and native porting work. Please do not:

- add proprietary game assets to the repo
- distribute copyrighted material without ownership or authorization
- submit code that violates the project’s legal or ethical constraints

If you are unsure about a contribution, open a discussion or an issue first.

## Questions

If you are unsure where to begin, start with an issue or a small documentation improvement. We appreciate contributions of all sizes.

Thank you for helping improve the project.
