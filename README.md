# suzume-sql

**suzume-sql** is just simple application to navigate your databases.

I wanted a simple and fast way to navigate my databases without having to use a full blown GUI application and without ever leaving the keyboard, so I made this application.

Suzume are small, nimble and efficient birds that are very common in Japan, and as I like birds and this application is just like them, I decided to name it suzume-sql (even though it is not as cute as them).

## Features

Currently the application packages mysql, postgres and sqlite3 (those are the only ones I have needed so far)

Some of the features are the following:

- Navigate the tables with a fuzzy finder.
- Fast querying/ordering by just writing the condition on the top pane.
- Transactional edit and removal in the grid view of tables.
- Copying cells or rows (as json) to the clipboard.
- Simple autocompletion for table names and columns.
- Inspecting a row or cell with proper formatting (json, timestamps, etc).

Currently mouse is not supported, but it is planned to be added in the future.

## Installation

The application is not uploaded to crates.io yet, but you can install from this repo with `cargo install --git https://github.com/Alexrp02/suzume-sql.git`.

## Usage

The first argument is a connection. If it carries a `scheme://` prefix it is used as a direct connection string (bypassing the config file); otherwise it is a connection name looked up in the config file:

```sh
suzume local                                      # connection named "local" in the config
suzume sqlite://./demo.db                         # sqlite (the sqlite:// prefix is required)
suzume postgresql://user:pass@localhost:5432/app  # postgres
suzume mysql://user:pass@localhost:3306/app       # mysql
```

With no argument, suzume-sql loads the config from the default config dirs of the different os (use -c/--config <path> to point it elsewhere).
When opening the application, a picker will show where you can select/create/modify/delete connections.

This is an example of a configuration file:

```toml
# suzume-sql.toml
[[connections]]
name = "local"
engine = "sqlite"
path = "./demo.db"

[[connections]]
name = "prod"
engine = "postgres"
url = "postgresql://user:pass@localhost:5432/app"

[[connections]]
name = "work"
engine = "mysql"
url = "mysql://user:pass@localhost:3306/app"
```

> [!NOTE]
> This application has been developed with AI and partially reviewed by a human (I have reviewed the vast majority, but some things haven't been reviewed so you can expect some parts of the code to be messier). I wanted to develop this application quickly, so everything ui related hasn't been deeply reviewed (just the surface of it, as I don't know ratatui and don't have the time to learn it right now). If you want to contribute, please feel free to do so! Even just an issue or directly a pull request will help making the application better.

## License

suzume-sql is free and open-source software, licensed under the **GNU General Public License v3.0 or later** (see [LICENSE](LICENSE)). You are free to use, study, modify, and redistribute it — including at work or in your business — and any distributed fork must remain under the same license.

## Contributing

Contributions are more than welcome! By submitting a pull request you agree that your contribution is licensed under the project's [GPL-3.0-or-later license](LICENSE) (inbound = outbound). See [CONTRIBUTING.md](CONTRIBUTING.md) to get started.
