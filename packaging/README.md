# Packaging

Release archives, the `.deb` and their `.sha256` files are built by
[`.github/workflows/release.yml`](../.github/workflows/release.yml) when a
tag `v*` is pushed. The tag must match the crate version (`v0.2.0` for
`0.2.0`); the workflow checks this. Each archive holds both binary names,
`baba-yaga` and `bay`, plus `LICENSE` and `baba-yaga.example.toml`.

| Target | Release file |
| --- | --- |
| macOS, Apple silicon | `baba-yaga-v<version>-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `baba-yaga-v<version>-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `baba-yaga-v<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Ubuntu / Debian x86_64 | `baba-yaga_<version>_amd64.deb` |

## Homebrew (macOS)

The formula is [`homebrew/baba-yaga.rb`](homebrew/baba-yaga.rb). It is not
published yet. To publish it:

1. Create a public GitHub repo named `shoaibyte/homebrew-tap`. Homebrew
   finds a tap by the `homebrew-` prefix.
2. Copy `packaging/homebrew/baba-yaga.rb` to `Formula/baba-yaga.rb` in that
   repo.
3. For the release, set `version` in the formula and replace each sha256
   placeholder with the hash from the matching `.sha256` file on the release
   page, for example:

   ```sh
   curl -sL https://github.com/shoaibyte/baba-yaga/releases/download/v0.2.0/baba-yaga-v0.2.0-aarch64-apple-darwin.tar.gz.sha256
   ```

   The first word of that file is the hash.
4. Commit and push the tap. Then anyone can install with:

   ```sh
   brew install shoaibyte/tap/baba-yaga
   ```

The formula depends on Homebrew's `osv-scanner`, so that is installed too.

## Ubuntu (.deb)

There is no apt repository yet: install the `.deb` from the release page.

```sh
curl -LO https://github.com/shoaibyte/baba-yaga/releases/download/v<version>/baba-yaga_<version>_amd64.deb
sudo apt install ./baba-yaga_<version>_amd64.deb
```

Check the download against `baba-yaga_<version>_amd64.deb.sha256` on the
same page (`sha256sum -c` after downloading both). The package puts
`baba-yaga` and `bay` in `/usr/bin` and the example config in
`/usr/share/doc/baba-yaga/`. It depends on `git`.

osv-scanner is not in Ubuntu's archive, so the package does not pull it in.
Install it from its release page or with Go:

```sh
go install github.com/google/osv-scanner/v2/cmd/osv-scanner@latest
```

`bay doctor` reports whether it is on `PATH`.

## After installing

In the directory that should hold `.env` and `baba-yaga.toml`:

```sh
bay init      # finds the server, registers the agent, opens addressing, picks the room, writes the config
bay doctor    # read-only checks, one PASS/FAIL/SKIP line each
bay serve
```
