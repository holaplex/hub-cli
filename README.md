# `hub-cli`

A command-line tool for interfacing with Holaplex Hub

---

`hub-cli` provides the `hub` command-line utility for interfacing with Hub from
shell scripts or directly from a terminal.

## Installation and Setup

<!-- TODO: add better install instructions here -->

### Prerequisites

To install `hub`, you'll need a Rust toolchain with Cargo, `clang`, and a copy
of the `protoc` Protobuf compiler. Additionally on POSIX systems you'll need
`pkg-config`, include files for OpenSSL, and on Linux the include files for
`liburing`.

The recommended way to install Cargo is [via rustup](https://rustup.org/).

On Debian-based Linux distributions, the additional dependencies can be
installed with APT:

```sh
$ sudo apt install clang libssl-dev pkg-config protobuf-compiler liburing-dev
```

On macOS the additional dependencies can be installed with Homebrew:

```sh
$ brew install clang openssl@1.1 pkg-config protobuf
$ brew info openssl@1.1 # follow the instruction to setup your terminal profile
```

To install `hub` from Git, simply check out the repo with `git clone`, and then
install it with the `--path` flag for `cargo install`:

```sh
$ git clone https://github.com/holaplex/hub-cli
$ cargo install --path hub-cli
```

### <a name="config-file"></a> Config locations

By default, `hub` creates a configuration file named `config.toml` in
a dedicated subdirectory of the current user's configuration directory. All
commands will read or write to this file. However, if the current directory in
which `hub` is run contains a file named `.hub-config.toml`, it will read and
write to that file instead. Additionally, the user can override the current
config location by passing `-C path/to/my-config.toml` to any `hub` command.

## Usage

For additional help with the command-line interface, you can always run:

```sh
$ hub help
```

This will print info about available commands and global options. For help with
a specific command, run:

```sh
$ hub help <COMMAND>
```

### `config`

The `config` command allows getting and setting various configuration options
of the [current config](#config-file):

```sh
$ hub config path # Print the path of the current config
$ hub config graphql-endpoint # Set the API endpoint to use
$ hub config token # Set the API token to use

$ hub config graphql-endpoint --get # Print the current API endpoint
```

### `upload drop`

This command populates an open drop by reading Metaplex metadata files and
associated asset files from a local directory:

```sh
$ hub upload drop --drop <DROP_ID> <INPUT_DIR...>
```

The `DROP_ID` parameter accepts the UUID of an open drop created with Hub, and
`INPUT_DIR` takes one or more directories containing JSON metadata files. All
assets specified using local paths in metadata files will be first uploaded to
the web using Hub's permaweb upload endpoint, and their permanent URL will be
used in place of a filesystem path. `hub` searches for asset paths relative to
the folder containing the JSON file referencing them; if any assets are in
separate folders, each folder can be specified with the `-I` flag:

```sh
$ hub upload drop --drop 00000000-0000-0000-0000-000000000000 \
    -I foo/assets1 \
    -I foo/assets2 \
    foo/json
```

When include paths are specified, `hub` first searches the current JSON
directory, then searches each include directory in the order they were
specified, stopping as soon as it finds a match.

### `airdrop`

This command airdrops NFTs by minting them from an open drop to specified wallet addresses.
This should be run after the mints are queued for the specified open drop.

```sh
$ hub airdrop --drop <DROP_ID> <WALLETS>
```

The `DROP_ID` parameter accepts the UUID of an open drop created with Hub. The --wallets parameter accepts file containing newline-separated wallet addresses for the airdrop. If a single hyphen - is passed, the utility will read from STDIN.

### Optional Parameters:

- `--concurrency <OPTIONS>`:
   - Is used to control the level of concurrency.

- `--no-compressed`:
   - An optional flag to mint uncompressed NFTs only.

- `--mints-per-wallet <NUMBER>`:
   - Defines the number of NFTs to mint to each wallet with a default value of 1 if not specified.


```sh
$ hub upload drop --drop 00000000-0000-0000-0000-000000000000 \
    --wallets /path/to/wallets.txt
    --concurrency 4 \
    --no-compressed \
    --mints-per-wallet 2
```

In this example, the airdrop command is invoked with a concurrency level of 4, specifying the open drop UUID 00000000-0000-0000-0000-000000000000, opting not to mint them as compressed NFTs, defining 2 mints per wallet, and providing the path to the file wallets.txt containing the target wallet addresses.
