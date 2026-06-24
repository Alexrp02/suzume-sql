# normal-sql

normal-sql is just a normal sql tui application for navigating your database.

I wanted a simple and fast way to navigate my databases without having to use a full blown GUI application and without ever leaving the keyboard, so I made this application.

## Features

Currently the application packages mysql, postgres and sqlite3 (those are the only ones I have needed so far)

Some of the features are the following:

- Navigate the tables with a fuzzy finder.
- Fast querying/ordering by just writing the condition on the top pane.
- Transactional edit in the grid view of tables.
- Copying cells or rows (as json) to the clipboard.
- Simple autocompletion for table names and columns.

## Installation

Currently the application is not uploaded to crates.io, so you can install from this repo with `cargo install --git https://github.com/Alexrp02/normal-sql.git`.

## Usage

The first argument is a connection. If it carries a `scheme://` prefix it is used as a direct connection string (bypassing the config file); otherwise it is a connection name looked up in the config file:

```sh
normal-sql local                                      # connection named "local" in the config
normal-sql sqlite://./demo.db                         # sqlite (the sqlite:// prefix is required)
normal-sql postgresql://user:pass@localhost:5432/app  # postgres
normal-sql mysql://user:pass@localhost:3306/app       # mysql
```

With no argument, normal-sql looks for `normal-sql.toml` in the current working directory (use -c/--config <path> to point it elsewhere). If the config defines more than one connection, an interactive picker is shown.

This is an example of a configuration file:

```toml
# normal-sql.toml
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
> This application has been generated with AI, so expect a lot of the code to be a bit messy and unoptimized (I haven't reviewed all of the code in detail). If you want to contribute, please feel free to do so.
