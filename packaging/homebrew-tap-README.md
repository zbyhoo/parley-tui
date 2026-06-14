# parley

Terminalowy interfejs (TUI) do równoległej pracy z agentami AI (`claude`, `codex`)
obok siebie, w jednym oknie.

## Wymagania

- macOS na Apple Silicon (arm64)
- [Homebrew](https://brew.sh)
- CLI agentów dostępne w `PATH`:
  - `claude` — [instalacja](https://docs.claude.com/claude-code)
  - `codex`

## Instalacja

```bash
brew install zbyhoo/parley/parley
```

Po instalacji `parley` jest dostępne w terminalu z dowolnego katalogu.

## Użycie

Uruchom w katalogu projektu:

```bash
parley
```

Skróty klawiszowe:

| Klawisz        | Akcja                          |
| -------------- | ------------------------------ |
| `Tab`          | przełącz aktywnego agenta      |
| `Enter`        | wyślij wiadomość do agenta     |
| `@all ...`     | wyślij do wszystkich agentów   |
| `?`            | pomoc                          |
| `Ctrl+R`       | restart aktywnego agenta       |
| `Ctrl+C` / `Ctrl+Q` | wyjście                   |

## Aktualizacja

```bash
brew upgrade parley
```

## Odinstalowanie

```bash
brew uninstall parley
brew untap zbyhoo/parley
```
