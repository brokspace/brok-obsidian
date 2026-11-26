<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/assets/brokspace-on-dark.svg">
    <img src=".github/assets/brokspace.svg" alt="Brokspace" height="44">
  </picture>
</p>

# brok-obsidian

A [Brokspace](https://github.com/brokspace/brok) shard that mirrors an
[Obsidian](https://obsidian.md) vault on **another machine or device**, through the community
[Local REST API](https://github.com/coddingtonbear/obsidian-local-rest-api) plugin, into a local,
**read-only** copy that `brok search`, `brok ask` and your coding agents can use.

> **Not released yet.** It is tested against a fake server and has not yet been run against the real plugin.
> If your vault already syncs to this machine (Obsidian Sync, Syncthing, git, iCloud), you do not need it:
> point `memory.obsidian.vault` at that folder.

## Set it up

You need brok 0.9 or newer (for `memory.obsidian.read_only`) and the plugin enabled in Obsidian.

1. Save the plugin's certificate authority into the workspace (from the machine running Obsidian,
   `<url>/obsidian-local-rest-api.crt`) as `.brokspace/obsidian-ca.crt`. Check that you got it from your own
   Obsidian: whoever you trust here receives your API key.
2. Put the API key (Obsidian > Settings > Local REST API) in the environment of `brok run`:
   `export OBSIDIAN_API_KEY=...`. Never write it in `brok.yml`.
3. Install and configure:

   ```sh
   brok shard install https://github.com/brokspace/brok-obsidian
   ```

   ```yaml
   shards:
     brok-obsidian:
       enabled: true
       settings:
         url: https://127.0.0.1:27124
         ca_file: .brokspace/obsidian-ca.crt
         exclude: [Private, "*.excalidraw.md"]
   memory:
     obsidian:
       vault: .brokspace/shards-data/brok-obsidian/vault
       search: vault
       read_only: true
   ```

4. `brok restart`. The vault is mirrored at start, every 15 minutes, and when you ask:
   `brok bus emit obsidian:sync '{}'`. Follow it with `brok bus tail 'obsidian:*'`.

### A vault on another machine

The plugin's certificate is only valid for `127.0.0.1`, `localhost`, its binding host and the extra names you
add in its settings. Prefer a private tunnel over opening the plugin to a network, for example
`ssh -L 27124:127.0.0.1:27124 you@desktop` and keep `url: https://127.0.0.1:27124`.

## Settings

| Setting | Default | Description |
|---|---|---|
| `url` | `https://127.0.0.1:27124` | Host and port of the plugin. HTTPS only. |
| `ca_file` | required | The plugin's certificate authority, relative to the workspace or absolute. |
| `exclude` | `[]` | Folders, notes or globs never downloaded. A name without `/` matches at any depth. |
| `interval_secs` | `900` | Seconds between syncs (at least 60); `0` syncs only at start and on request. |
| `timeout_secs` | `20` | Per request. |
| `max_notes` | `20000` | |
| `max_note_bytes` | `524288` | Larger notes are skipped. |
| `max_total_bytes` | `67108864` | |
| `max_depth` | `16` | Folder depth. |

## What it does and does not do

- **Only reads.** The shard sends nothing but `GET` requests, although the API key could change or delete
  notes. It never writes to your vault.
- **Trusts one certificate.** Only your `ca_file`; there is no option to skip verification. No redirects, no
  proxy, requests only to `url`.
- **Keeps the key out of sight.** It is read from `OBSIDIAN_API_KEY` and never written or logged. Logs and
  events hold counts, not note names or contents.
- **Copies only Markdown notes**, never hidden folders such as `.obsidian/`, and refuses file names that could
  leave the mirror or that some system cannot store.
- **Keeps the last good copy** when the plugin is offline or refuses the key, and removes notes only after a
  complete walk of the vault.
- **Every sync asks for every note.** A server that answers "not modified" saves the transfer; otherwise the
  note is downloaded and written only if it changed. Raise `interval_secs` for very large vaults.
- The copy is your notes in plain text under `.brokspace/`. Use `exclude` for what must not be there.

Brokspace's shard sandbox is advisory: these limits are enforced by this shard's code and tests. The design and
threat model are in [brok's ADR 0016](https://github.com/brokspace/brok/blob/staging/docs/adr/0016-remote-obsidian-vaults-by-mirroring.md).

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## License

MIT or Apache-2.0, at your option.
