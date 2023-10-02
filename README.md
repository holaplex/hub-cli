# `hub-cli`
A command-line tool for interfacing with Holaplex Hub

---

`hub-cli` provides the `hub` command-line utility for interfacing with Hub from
shell scripts or directly from a terminal.

## Installation and Setup

<!-- TODO: add better install instructions here -->

To install `hub`, the only prerequisite you'll need is a Rust toolchain with
Cargo.

To install `hub` from Git, simply check out the repo with `git clone`, and then
install it with the `--path` flag for `cargo install`:

```sh
$ git clone https://github.com/holaplex/hub-cli
$ cargo install --path hub-cli
```

### <a name="config-file"></a> Config locations

By default, `hub` creates a configuration file named `config.toml` in
a dedicated subdirectory of the current user's configuration directory.  All
commands will read or write to this file.  However, if the current directory in
which `hub` is run contains a file named `.hub-config.toml`, it will read and
write to that file instead.  Additionally, the user can override the current
config location by passing `-C path/to/my-config.toml` to any `hub` command.

## Usage

For additional help with the command-line interface, you can always run:

```sh
$ hub help
```

This will print info about available commands and global options.  For help with
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
`INPUT_DIR` takes one or more directories containing JSON metadata files.  All
assets specified using local paths in metadata files will be first uploaded to
the web using Hub's permaweb upload endpoint, and their permanent URL will be
used in place of a filesystem path.  `hub` searches for asset paths relative to
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
