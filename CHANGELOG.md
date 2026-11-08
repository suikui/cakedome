# Changelog

All notable changes to this project are documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-11-08

### Fixed

- `save_with(..).belongs_to(..)` inserted a target whose primary key was `Unchanged` (for example
  an `ActiveModel` made from a loaded `Model`), which failed with a duplicate key error. Like every
  other save path, only a `NotSet` primary key now inserts; an `Unchanged` one updates the row.
- The panic message for a many-to-many whose owner does not have a single `id` primary key now
  names the owner `parent` instead of `target`.

### Changed

- `Registry::verify` now also checks built-in declarations (`BelongsToOf`, `HasManyOf`,
  `HasOneOf`, `ManyToManyOf`) added with `RepoConfig::add_association`: their Repo types must
  match the registered Repo types, and their single `id` primary key is checked as with
  `association`. A registration that passed `verify` in 0.1.0 with mismatched types now panics at
  startup.

## [0.1.0] - 2026-11-08

First release.

[0.1.1]: https://github.com/suikui/cakedome/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/suikui/cakedome/releases/tag/v0.1.0
