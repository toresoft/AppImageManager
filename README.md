# AppImage Manager

Un gestore di AppImage **nativo per KDE**, scritto in Rust.
Al click su un file `.AppImage` in Dolphin appare una finestra di conferma
(`kdialog`); se accetti, l'AppImage viene **copiata in `~/.local/bin`**, viene
creata la **voce nel menù di KDE** (icona + `.desktop`) e l'app viene
**avviata**. Include anche `list` e `uninstall`.

Stato: progetto greenfield, funzionante e testato su KDE Plasma con un'AppImage
reale (ZCode).

## Caratteristiche

- **Click-to-install**: il click su un'AppImage in Dolphin apre la conferma,
  poi installa e avvia (come AppImageLauncher, ma minimale e KDE-native).
- **Integrazione menù completa**: copia l'eseguibile, riscrive il `.desktop`
  con il path assoluto, installa tutte le icone hicolor, aggiorna la cache
  delle icone e il database dei `.desktop`.
- **List / Uninstall**: elenca le AppImage installate e rimuovile (binario,
  voce di menù e icone) con conferma.
- **Niente esecuzione dell'AppImage durante l'installazione**: i metadati
  (`.desktop`, icone) sono estratti leggendo direttamente il payload squashfs
  tramite `unsquashfs`, mai eseguendo il file.
- **Zero runtime pesanti**: solo Rust + `kdialog`. Nessun binding Qt.

## Requisiti

Runtime (già presenti su una KDE Plasma standard):

- `kdialog`
- `unsquashfs` (pacchetto `squashfs-tools`)
- `xdg-mime`, `xdg-icon-resource` (pacchetto `xdg-utils`)
- `update-desktop-database`, `gtk-update-icon-cache`

Build:

- Rust ≥ 1.85 (edition 2024). Installa con [`rustup`](https://rustup.rs) o il
  pacchetto della tua distro (`rust` + `cargo`).

## Build

```bash
cargo build --release
# binario: target/release/app-image-manager
```

## Installazione

Copia il binario in una directory nel `PATH` (utente):

```bash
mkdir -p ~/.local/bin
cp target/release/app-image-manager ~/.local/bin/
```

Assicurati che `~/.local/bin` sia nel `PATH` (di solito lo è su KDE Plasma).

## Setup (una tantum)

Registra il tool come handler predefinito per i MIME type delle AppImage,
così il click in Dolphin apre la finestra di conferma:

```bash
app-image-manager setup
```

Questo scrive `~/.local/share/applications/appimage-handler.desktop` (con
`Exec=...app-image-manager handle %f`) e lo registra come default per
`application/vnd.appimage`, `application/x-appimage` e (come fallback)
`application/octet-stream` tramite `xdg-mime`.

> **Nota su Dolphin**: Dolphin può avere l'azione "Esegui" per i binari
> eseguibili. Affinché il **click singolo** apra la conferma di installazione
> (e non esegua direttamente l'AppImage), il MIME type del file deve risultare
> `application/vnd.appimage` o `application/x-appimage`. Se Dolphin mostra
> "Tipo: sconosciuto" o "eseguibile", rigenera il database MIME o usa
> *Apri con → AppImage Manager* la prima volta, quindi imposta
> "Ricorda l'associazione". In alternativa, imposta Dolphin per aprire i file
> con un singolo click (Impostazioni di Dolphin → Comportamento).

## Utilizzo

```bash
# Click in Dolphin → equivalente a:
app-image-manager handle /percorso/al/App.AppImage

# Installazione non interattiva (script/CLI)
app-image-manager install /percorso/al/App.AppImage

# Elenca le AppImage installate
app-image-manager list

# Disinstalla (con conferma kdialog)
app-image-manager uninstall <nome>
# Disinstalla senza prompt (script)
app-image-manager uninstall <nome> --yes
```

Dove `<nome>` è mostrato da `list` (es. `zcode`), derivato dal campo `Name`
del `.desktop` interno, normalizzato in minuscolo senza spazi.

## Dove finiscono i file

Tutto sotto la home dell'utente (scope per-utente, niente `sudo`):

| Cosa | Dove |
|---|---|
| Eseguibile AppImage | `~/.local/bin/<nome>.AppImage` (permessi `0700`) |
| Voce di menù | `~/.local/share/applications/<nome>.desktop` |
| Icone | `~/.local/share/icons/hicolor/<size>x<size>/apps/<nome>.png` |
| Handler MIME | `~/.local/share/applications/appimage-handler.desktop` |

Le voci di menù create da questo tool sono marcate con
`X-AppImage-Manager=true` (più `X-AppImage-Source=<percorso originale>`), così
`list` e `uninstall` riconoscono solo quelle gestite dal tool e non toccano
altre entry.

## Come funziona (brevemente)

1. **Riconoscimento**: legge i magic byte dell'ELF + `AI\x02` (AppImage type 2).
2. **Offset del payload**: cerca il magic squashfs `hsqs` nel file e **valida**
   ogni candidato contro il superblock squashfs (`block_size == 1 << block_log`,
   `bytes_used` coerente con la dimensione del file). Questo evita i falsi
   positivi quando `hsqs` compare nei dati dell'ELF.
3. **Estrazione mirata**: usa `unsquashfs -o <offset> -cat` per estrarre solo
   il `.desktop` radice e le icone hicolor, senza spacchettare tutto né
   eseguire l'AppImage.
4. **Reinstallazione**: copia l'eseguibile, riscrive `Exec=AppRun ...` in
   `Exec=~/.local/bin/<nome>.AppImage ...`, aggiunge i marker, installa le
   icone via `xdg-icon-resource --novendor` (con fallback a copia manuale),
   aggiorna i database.
5. **Avvio**: lancia l'AppImage installata in una nuova sessione (`setsid`)
   così sopravvive alla chiusura del processo chiamante.

## Struttura del progetto

```
src/
├── main.rs        # entrypoint + dispatch comandi
├── cli.rs         # definizione CLI (clap)
├── appimage.rs    # riconoscimento type 2 + offset squashfs (con validazione)
├── metadata.rs    # estrazione .desktop + icone via unsquashfs
├── desktop.rs     # parser/serializer .desktop (INI minimale)
├── installer.rs   # copia, rewrite .desktop, icone, refresh DB, list, uninstall
├── launcher.rs    # avvio non bloccante (setsid)
├── kdialog.rs     # wrapper kdialog (yesno/msgbox/error/...)
└── mime.rs        # setup handler MIME
```

## Pacchettizzazione (CI e pacchetti)

Il progetto produce due pacchetti nativi per x86_64 (amd64):

- **`.deb`** per Ubuntu/Debian — via [`cargo-deb`](https://github.com/kornelski/cargo-deb)
- **`.rpm`** per Fedora 44 — via [`cargo-generate-rpm`](https://github.com/cat-in-136/cargo-generate-rpm), costruito in container `fedora:44` per il glibc corretto

La GitHub Action `.github/workflows/release.yml` ha questa pipeline:

| Job | Runner | Cosa fa |
|---|---|---|
| `build` | `ubuntu-22.04` | fmt + clippy (`-D warnings`) + test + build release |
| `deb` | `ubuntu-22.04` | `cargo deb` → `app-image-manager_*_amd64.deb` |
| `rpm` | `fedora:44` (container) | `cargo generate-rpm` → `app-image-manager_*_x86_64.rpm` |
| `release` | `ubuntu-22.04` | solo su tag `v*`: crea GitHub Release con `.deb` + `.rpm` |

I pacchetti installano:

- `/usr/bin/app-image-manager` (binario, `0755`)
- `/usr/share/applications/appimage-handler.desktop` (handler MIME di sistema, punta a `/usr/bin/app-image-manager handle %f`)
- `/usr/share/metainfo/app-image-manager.metainfo.xml` (AppStream, per Discover/GNOME Software)

Con le dipendenze runtime dichiarate (`kdialog`, `squashfs-tools`, `xdg-utils`, `desktop-file-utils`, `gtk-update-icon-cache`).

### Scaricare i pacchetti

- **Da una release**: vai nella pagina *Releases* del repo e scarica il `.deb` o `.rpm` dell'ultima versione.
- **Da un run CI (qualsiasi push/PR)**: nella scheda *Actions* del repo, apri il run, scorri fino ad *Artifacts* e scarica `app-image-manager-amd64-deb` o `app-image-manager-x86_64-rpm`.

### Installazione dai pacchetti

```bash
# Ubuntu/Debian
sudo apt install ./app-image-manager_*_amd64.deb

# Fedora 44
sudo dnf install ./app-image-manager_*_x86_64.rpm
```

Con il pacchetto installato **non serve** eseguire `app-image-manager setup`: la registrazione MIME avviene automaticamente tramite gli script di post-installazione del pacchetto.

### Registrazione MIME automatica (post-install / pre-uninstall)

I pacchetti `.deb` e `.rpm` registrano e deregistrano l'handler MIME tramite i rispettivi script maintainer, che invocano un helper condiviso (`/usr/lib/app-image-manager/mime-register.sh`):

| Evento | Debian | RPM | Azione |
|---|---|---|---|
| Installazione | `postinst configure` | `%post` | registra i MIME type in `/usr/share/applications/mimeapps.list` |
| Upgrade | `postinst configure` (nuovo) | `%post` (idempotente) | refresh della registrazione |
| Rimozione | `prerm remove` | `%preun` (con `$1 == 0`) | deregistra i MIME type |
| Dopo rimozione | — | `%postun` | refresh di `update-desktop-database` / `gtk-update-icon-cache` |

Note:
- Su **upgrade** la registrazione viene mantenuta (non rimossa in `prerm`/`%preun`), così il nuovo pacchetto la rinfresca senza "sfarfallii".
- La deregistrazione avviene in `prerm`/`%preun` (prima della cancellazione dei file), non in `postrm`/`%postun`, perché l'helper deve essere ancora presente sul disco.
- L'helper è **idempotente**: marca il proprio blocco con `# BEGIN/END app-image-manager` e modifica solo le righe dei propri MIME type, preservando ogni altra associazione presente nel file.

I MIME type registrati come default: `application/vnd.appimage`, `application/x-appimage`, `application/octet-stream` (come fallback).

### Build locale dei pacchetti

```bash
cargo install cargo-deb cargo-generate-rpm --locked
cargo deb                      # → target/debian/*.deb
cargo build --release          # richiesto prima del generate-rpm
cargo generate-rpm             # → target/generate-rpm/*.rpm
```

> Nota: il `.rpm` va costruito su Fedora (o nel container `fedora:44`) per avere il glibc giusto; costruendolo altrove si ottiene un pacchetto che al limite non si installa su Fedora 44.

## Sviluppo

```bash
cargo test          # 7 test unitari
cargo clippy --all-targets -- -D warnings
cargo build --release
```

## Limiti attuali (roadmap)

- **Niente update via zsync**: non ancora supportato (AppImageUpdate).
- **Scope per-utente**: installa solo in `~/.local`, non a livello di sistema.
- **MIME detection in Dolphin**: dipende dal fatto che il file sia classificato
  come `application/*appimage`; vedi la nota nel paragrafo *Setup*.

## Licenza

MIT.
