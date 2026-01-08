# Contribution guidelines

First off, thank you for considering contributing to this project.

Please follow these guidelines for helping us to better address your issue, assessing changes, and helping you finalize your pull requests. There are many ways to contribute, from writing examples, improving the documentation, submitting bug reports and feature requests or writing code which can be incorporated into the module itself.

## Code of conduct

This project adheres to the [Contributor Covenant Code of Conduct](./CODE_OF_CONDUCT.md), please read it and follow it before contributing. If you find someone that is not respecting it please report its behavior.

## Project overview

This project implements a [Model Context Protocol](https://modelcontextprotocol.io/docs/getting-started/intro) (MCP) server for Mia-Platform Catalog, enabling AI clients to interact with Catalog resources through natural language.

## Development setup

### Prerequisites

- [Rust](https://rust-lang.org/tools/install/)
- [Cargo Make](https://github.com/sagiegurari/cargo-make) (`cargo install --force cargo-make`)

### Getting started

#### 1. Clone the repository

```sh
git clone git@github.com:mia-platform/catalog-mcp-server.git
cd catalog-mcp-server
```

#### 2. Setup the development environment

The `.dev/` directory contains the configuration needed to spin up a development environment using [Docker Compose](https://docs.docker.com/compose/).

> [!WARNING]
> To pull the some of the images you need to be logged in the Mia-Platform private registry.

The stack is composed of:

- an instance of MongoDB (exposed locally on port `27017`), and
- an instance of the Catalog Engine (exposed locally on port `3000`).

The playground comes with two organizations, `system` and `org_1`, and the database is already packed with some mock data.

To spin up the system, run:

```sh
cargo make dev_up
```

To teardown the playground, run

```sh
cargo make dev_down
```

#### 3. Add the MCP server to your IDE of choice

> [!TIP]
> See the [README](./README.md) for detailed client integration instructions.

```json
{
  "servers": {
    "catalog": {
      "type": "stdio",
      "command": "cargo",
      "args": [
        "run",
        "--",
        "--stdio",
        "--base-url=http://localhost:3000"
      ]
    }
  },
  "inputs": []
}
```

## Code contribution workflow

1. Create a new branch for you feature or bugfix:

   ```sh
   git checkout -b feat/your-feature-name
   ```

2. Make your changes, following the code style of the project.

3. Commit your changes using [conventional commits](https://www.conventionalcommits.org/en/v1.0.0/) format.

### Linting

Linting of project code is performed using `rustfmt` with a customized _nightly_ rule for imports and [`clippy`](https://github.com/rust-lang/rust-clippy).

Consequently, it is necessary to install Rust nightly channel:

```shell
rustup toolchain install nightly
```

and introduce `clippy` as an additional component to the toolchains:

```shell
rustup component add clippy
```

Afterward, it is possible to lint the project with the following commands:

```shell
cargo +nightly fmt --check
cargo clippy --all-targets
```

### Pull Request Guidelines

For pull requests titles always follow the [Conventional Commit](https://www.conventionalcommits.org/en/v1.0.0/) standard of `<type>(<scope>): <subject>` (e.g. `feat(core): Add new feature`).

Pull request titles and labels are automatically used as entries in the release note, so please pay extra attention when setting one up. You can find the complete mapping between pull request labels and release note categories in the `.github/release.yml` file. Unlabeled PRs will be automatically assigned the `Other changes` category in the release note.

## Release Process

_Coming soon..._

## License

By contributing to this project, you agree that your contributions will be licensed under the project's license.

## Questions?

If you have any questions or need help, please open an issue or reach out to the maintainers.
