# Samples

These projects pin known-vulnerable dependency versions on purpose, as deterministic input for `baba-yaga scan --path samples`. They must never be deployed or used as a starting point for real code.

- `go/`: golang.org/x/text and golang.org/x/net
- `python/`: requests, urllib3 and jinja2
- `node/`: lodash, minimist and axios
- `php/`: symfony/http-foundation and guzzlehttp/guzzle
