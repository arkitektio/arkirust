# Arkitekt goes rust
Welcome to the Arkirust project! This repository is dedicated to implementing the Arkitekt project using the Rust programming language.

## Overview
The goal of this project is to provide a minimum example of an arkitekt enabled rust application that provides a bit of bioimage functionatily.. In its current version it also provide a vary barebone implementation of what an application needs in order to communicate with arkitekt.

Features:

- [x] Fakts config retrieval (only device code grant possible)
- [x] Oauth2 token retrievel (no refresh yet)
- [x] Typed Queries through the GraphQL-Client codegen
- [x] Function invokation through as a Rekuest Agent 
- [x] Arkitekt Node registration trough the GraphQL APi
- [ ] Automatic Macro based function registration

Roadmap:

- [ ] Zarr-rust based uploads to Arkitekt


## Getting Started
To get started with Arkirust, you'll need to have Rust installed on your machine. You can follow the official Rust installation guide here.
Once Rust is installed, you can clone this repository and build the project using the following commands:

```bash
sh
git clone https://github.com/jhnnsrs/arkirust.git
cd arkirust
cargo build
cargo run
```

## Contributions
Contributions are welcome! If you have any ideas, suggestions, or improvements, feel free to open an issue or submit a pull request.

## Recent Updates
The repository is actively maintained, with recent updates focusing on improving the core functionality and adding new features. For more details, you can check the commit history.

