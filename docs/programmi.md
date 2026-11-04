# Programmi per arcOS

Un programma arcOS è un file `.app`: un eseguibile ELF64 che gira in
modalità utente (ring 3), nel suo spazio di indirizzi. Parla con il kernel
solo attraverso le chiamate di sistema (`int 0x80`). Il kernel lo carica a
`0x400000`, gli dà uno stack sotto `0x7FFF_FFF0_0000` e la memoria che
chiede a partire da `0x1000_0000_0000`.

Se il programma fa un accesso non valido, il kernel lo ferma e lo segnala.
Il resto del sistema continua normalmente. Ctrl+C ferma il programma in
primo piano.

## Avviarli

La shell cerca i programmi in `sys:/apps`, quindi basta il nome:

```
hello mondo
calc 6 * 7
edit sys:/home/nota.txt
run sys:/apps/info.app
ps                       programmi e thread in esecuzione
kill <pid>
```

I programmi si possono scrivere anche in ArcScript: la shell avvia i
sorgenti `.as` e i pacchetti `.arc` attraverso `arcc`, e cerca anche
questi in `sys:/apps` (`notes` è `sys:/apps/notes.as`). Vedi
[arcscript.md](arcscript.md).

Oltre ai programmi nativi, arcOS avvia anche **programmi Linux e Windows**
per x86-64: un eseguibile ELF collegato staticamente è un programma Linux, un
`.exe` è un programma Windows (avviato da `winrun`). Il comando `install` copia
programmi liberi già pronti dal negozio `sys:/store` (per esempio BusyBox e
Lua). Tutto questo è descritto in [compat.md](compat.md).

## Scriverne uno

Ogni cartella in `system/apps/` è un programma. Il `Makefile` la compila e
la copia nel volume di sistema come `sys:/apps/<nome>.app`.

`Cargo.toml`:

```toml
[package]
name = "saluto"
description = "arcOS program: saluto"
version = "1.0.0"
edition.workspace = true
publish.workspace = true
authors.workspace = true

[[bin]]
name = "saluto"
path = "src/main.rs"
test = false
bench = false

[dependencies]
arcrt.workspace = true

[lints]
workspace = true
```

`build.rs`:

```rust
fn main() {
    println!("cargo:rustc-link-arg-bins=-e");
    println!("cargo:rustc-link-arg-bins=_start");
    println!("cargo:rustc-link-arg-bins=--strip-all");
    println!("cargo:rustc-link-arg-bins=-z");
    println!("cargo:rustc-link-arg-bins=max-page-size=4096");
}
```

`src/main.rs`:

```rust
#![no_std]
#![no_main]

extern crate alloc;

use arcrt::console::color::*;
use arcrt::println;

arcrt::main!(main);

/// `args` è il resto della riga di comando. Il valore restituito è il
/// codice d'uscita.
fn main(args: &str) -> i32 {
    let nome = args.trim();
    let nome = if nome.is_empty() { "mondo" } else { nome };
    println!("{ACCENT}Ciao, {nome}!{RESET}");

    // File: percorsi con il volume ("sys:/home/...") o relativi alla
    // cartella corrente.
    let testo = alloc::format!("salutato {nome}\n");
    if let Err(e) = arcrt::fs::write("sys:/home/saluti.txt", testo.as_bytes()) {
        println!("non riesco a scrivere: {e:?}");
        return 1;
    }
    0
}
```

`arcrt` contiene anche l'allocatore (si possono usare `Vec`, `String`,
`Box`) e il gestore dei panic.

### La libreria `arcrt`

| Modulo | Contenuto |
|---|---|
| `console` | `print!`, `println!`, `eprintln!`, `read_key`, `poll_key` (con timeout), `read_line`, `size`, `clear`, `goto`, colori in `console::color` |
| `fs` | `File` (`open`, `create`, `append`, `read`, `write`, `seek`, `size`, `read_to_end`), `read`, `read_to_string`, `write`, `list_dir`, `make_dir`, `remove`, `rename`, `current_dir`, `set_current_dir` |
| `process` | `exit`, `spawn`, `wait`, `yield_now`, `sysinfo` |
| `time` | `uptime_ms`, `sleep_ms`, `now`, `date` |
| `ipc` | `Port` (`create`, `lookup`, `send`, `recv`, `call`), messaggi tra programmi |
| `app` | `id`, `data_dir`, `data_file`, `pick_open`, `pick_save`, `request`, `permission`: identità e sandbox |
| radice | `main!`, `split_args` (argomenti, con le virgolette) |

### Le chiamate di sistema

Numero in `rax`, argomenti in `rdi`, `rsi`, `rdx`, `r10`, `r8`; il
risultato torna in `rax` (negativo = errore, vedi `arcdk::syscall::Error`).
File e cartelle aperti sono **handle** numerici, non descrittori Unix.

| N. | Nome | Cosa fa |
|---|---|---|
| 0 | `EXIT` | termina il programma |
| 1 | `WRITE` | scrive su un handle (la console o un file) |
| 2 | `READ_KEY` | legge un tasto (con attesa massima) |
| 3 | `OPEN` | apre o crea un file, restituisce un handle |
| 4 | `READ` | legge da un handle |
| 5 | `CLOSE` | chiude un handle |
| 6 | `SEEK` | sposta la posizione |
| 7 | `FILE_SIZE` | dimensione del file |
| 8 | `LIST_DIR` | contenuto di una cartella |
| 9 | `REMOVE` | cancella un file o una cartella vuota |
| 10 | `MAKE_DIR` | crea una cartella |
| 11 | `UPTIME` | millisecondi dall'avvio |
| 12 | `SLEEP` | attende |
| 13 | `NOW` | data e ora (secondi dal 2000) |
| 14 | `SPAWN` | avvia un altro programma |
| 15 | `WAIT` | attende la fine di un programma |
| 16 | `YIELD` | cede il processore |
| 17 | `MAP` | aggiunge memoria allo spazio del programma |
| 18 | `SYSINFO` | memoria, processore, versione |
| 19 | `SCREEN` | dimensione della console, cursore, pulizia |
| 20 | `RENAME` | rinomina o sposta |
| 21 | `GET_CWD` | cartella corrente |
| 22 | `SET_CWD` | cambia la cartella corrente |
| 23 | `SET_TLS` | base di FS o GS del thread (memoria locale al thread) |
| 24 | `MAP_AT` | memoria a un indirizzo fisso |
| 25 | `UNMAP` | restituisce memoria |
| 26 | `WIN_CREATE` | apre una finestra sulla scrivania |
| 27 | `WIN_PRESENT` | mostra un fotogramma della finestra |
| 28 | `WIN_EVENT` | il prossimo evento (mouse, tastiera, chiusura, nuova misura) |
| 29 | `WIN_CLOSE` | chiude la finestra |
| 30 | `WIN_TITLE` | cambia il titolo |
| 31 | `PORT_CREATE` | crea una porta di messaggi (con nome o privata) |
| 32 | `PORT_LOOKUP` | trova una porta per nome |
| 33 | `PORT_SEND` | invia un messaggio |
| 34 | `PORT_RECV` | riceve un messaggio, con il mittente e dove rispondere |
| 35 | `PORT_CALL` | invia una richiesta e aspetta la risposta |
| 36 | `PROC_NAME` | il nome di un processo |
| 37 | `APP_ID` | l'`app_id` del programma (vuoto per i programmi di sistema) |
| 38 | `APP_ENTER` | entra nella sandbox di un'app (una volta sola, senza ritorno) |
| 39 | `PICK_FILE` | il selettore di file del sistema (`PICK_OPEN` o `PICK_SAVE`) |
| 40 | `WIN_DROPPED` | i file dell'ultimo rilascio sulla finestra, uno per riga |

Due errori riguardano la sandbox: `Denied` (permesso negato) e `Cancelled`
(l'utente ha chiuso il selettore).

## Messaggi tra programmi

Un programma offre un servizio creando una porta con un nome; gli altri la
trovano e le scrivono. `arcrt::ipc` lo nasconde:

```rust
use arcrt::ipc::Port;

// il servizio
let porta = Port::create("org.esempio.eco")?;
loop {
    let m = porta.recv(None)?;          // aspetta un messaggio
    m.reply(m.text().to_uppercase().as_bytes())?;
}

// un altro programma
let eco = Port::lookup("org.esempio.eco")?;
let risposta = eco.call(b"ciao")?;      // b"CIAO"
```

Il kernel dice a chi riceve il pid di chi ha scritto (`m.sender`), che un
programma non può falsificare. Il sistema offre `arc.notify`: un messaggio
`{"title": "…", "text": "…"}` mostra una notifica sulla scrivania. Dalla
shell: `ipc notify Titolo testo`, `ipc call <porta> <testo>`, `ports`.

Un'app nella sandbox può creare solo porte con il proprio nome
(`<app_id>` o `<app_id>.qualcosa`).

## Sandbox delle app

Un programma installato in `sys:/apps` con un `<nome>.info` di tipo `native`
(come quelli del Negozio) è un'**app**: ha un `app_id`, dalla chiave
`app_id=org.esempio.saluto` del `.info` o, se manca, `org.arcos.<nome>`. I
pacchetti `.arc` lo prendono dal manifest. Gli strumenti di sistema
(`sys:/apps/*.app` senza `.info`) non sono nella sandbox. I processi avviati
da un'app restano nella sua sandbox.

Un'app può:

- leggere e scrivere nella propria cartella `sys:/home/.apps/<app_id>`, che è
  anche la cartella di partenza (`arcrt::app::data_file("record")`);
- leggere le app, i caratteri, i certificati, i suoni e le icone di sistema;
- usare le cartelle della home (`files:Immagini`, `files:Documenti`…) o un
  altro disco (`files:ram:`) dopo che l'utente ha detto sì: la prima volta
  il sistema mostra la domanda e sospende la chiamata;
- aprire o salvare il file che l'utente sceglie nel selettore, senza altri
  permessi:

```rust
match arcrt::app::pick_save("Disegno.qoi") {
    Ok(percorso) => arcrt::fs::write(&percorso, &dati)?,
    Err(Error::Cancelled) => {}            // l'utente ha annullato
    Err(e) => return Err(e),
}
```

Il resto restituisce `Denied`. Fotocamera, microfono, posizione e rete si
chiedono al servizio `arc.permissions`: `arcrt::app::request("camera")`
restituisce `Ok(true)` se l'utente consente, `permission("camera")` dice la
risposta già data senza chiedere. Le risposte stanno in
`sys:/system/permissions.json`; si cambiano in *Impostazioni → Sistema →
Privacy delle app* o con `perms` nel terminale (`perms allow
org.arcos.disegno files:Immagini`, `perms forget <app>`). Per provare un
programma nella sandbox: `sandbox run <app_id> <programma>`.

I programmi Linux e Windows non sono (ancora) nella sandbox.

## Programmi con una finestra

Un programma può aprire una o più finestre sulla scrivania, che si comportano
come quelle delle app di sistema: titolo, pulsanti, dock, Alt+Tab, animazioni.
Il programma disegna il contenuto nella propria memoria, con la stessa
libreria di disegno del sistema (`arcgfx`: forme, testo nei caratteri di
sistema, immagini QOI), e lo consegna fotogramma per fotogramma; il kernel lo
compone sopra il vetro smerigliato della finestra. Le parti lasciate
trasparenti mostrano il vetro.

La libreria `arcui` fa tutto questo:

```rust
#![no_std]
#![no_main]

extern crate alloc;

use arcui::{ui, Event, Font, Key, Rect, Window};

arcrt::main!(main);

fn main(_args: &str) -> i32 {
    let Ok(mut win) = Window::new("Contatore", 320, 200) else { return 1 };
    let mut n = 0;
    loop {
        win.clear(); // trasparente: si vede il vetro
        win.text_centered(Font::Display, 160, 40, &alloc::format!("{n}"), ui::TEXT);
        let f = win.fonts;
        ui::button(&mut win.canvas, f, Rect::new(110, 130, 100, 36), "+1", true, false);
        win.present();
        match win.wait() {
            Event::Press { x, y, .. } if Rect::new(110, 130, 100, 36).contains(x, y) => n += 1,
            Event::Key(k) if k.key == Key::Char(' ') => n += 1,
            Event::Close => return 0,
            _ => {}
        }
    }
}
```

| `arcui` | |
|---|---|
| `Window::new`, `Window::resizable` | apre una finestra con l'area del contenuto di quella misura |
| `Window::with_flags(…, RESIZABLE \| DROPS)` | con le opzioni scelte; `DROPS`: accetta i file trascinati |
| `win.dropped()` | i percorsi dell'ultimo `Event::Drop` |
| `win.canvas` | dove si disegna (`arcgfx::Canvas`: `fill`, `fill_round`, `line`, `arc`, `triangle`, `draw_image`, `blit`…) |
| `win.text`, `win.text_centered`, `win.fonts` | testo nei caratteri di sistema |
| `win.present()` | mostra il fotogramma |
| `win.wait()`, `win.event(ms)` | il prossimo evento, con o senza limite di tempo |
| `Event` | `Press`, `Menu` (tasto destro), `Release`, `Move`, `Leave`, `Key`, `Scroll`, `Resize`, `Close`, `Drop` |
| `ui` | i colori e alcuni controlli delle app di sistema (`button`, `segmented`, `overlay`) |
| `Rng`, `now_ms` | numeri casuali e tempo, per i giochi |

Quando l'utente chiude la finestra arriva `Event::Close`: il programma
dovrebbe finire; se dopo due secondi è ancora lì, il kernel lo ferma. Se il
programma si ferma per un errore, la finestra si chiude e una notifica lo
dice.

Una finestra aperta con `DROPS` riceve i file trascinati da File: arriva
`Event::Drop { x, y, count }` e `win.dropped()` dà i percorsi. Un'app nella
sandbox può leggere quei file (non scriverli: per salvare usa
`pick_save`). Le cartelle non arrivano. Disegno apre così le immagini:

```rust
let mut win = Window::with_flags("Disegno", 640, 480, arcui::DROPS)?;
// ...
Event::Drop { .. } => {
    if let Some(p) = win.dropped().into_iter().find(|p| p.ends_with(".qoi")) {
        apri(&p);
    }
}
```

Le app del Negozio (`system/store/`: 2048, Campo minato, Serpente, Sudoku,
Tris, Disegno) sono programmi di questo tipo e un buon punto di partenza.
Per metterne uno nel Negozio basta una cartella `system/store/<nome>` con il
programma, un file `info` e un'icona `icon.qoi` di 128 pixel: il `Makefile`
li copia in `sys:/store/<nome>`. Nel file `info` la riga
`permissions=files:Immagini,camera` dichiara i permessi che l'app potrà
chiedere: il Negozio li mostra nella sua pagina.

### Aggiornamenti delle app

Il servizio `arc.updates` (`kernel/src/appupdate.rs`) controlla una volta al
giorno un indice firmato con Ed25519, scarica in background le versioni
nuove delle app installate e le mette al posto delle vecchie appena sono
chiuse. Se un aggiornamento chiede permessi nuovi, aspetta che l'utente lo
approvi nella sezione *Aggiornamenti* del Negozio, dove si può anche
spegnere l'aggiornamento automatico. Dal terminale: `updates`, `updates
check`, `updates install <app>|all`, `updates auto on|off`.

Per pubblicare una versione nuova: si alza `version=` nel file `info`, poi

```sh
make updates                    # build/updates: index.json, la firma, le app
scripts/publish-updates.sh      # le mette sul ramo updates del repository
```

La chiave privata sta fuori dal repository (`~/.arcos-keys/updates.key`,
32 byte in esadecimale; `UPDATES_KEY=` per un'altra) e la pubblica è in
`appupdate.rs` (`arctool pubkey <chiave>` la stampa). Ogni indice ha un
numero di serie che cresce: arcOS rifiuta un indice firmato male o più
vecchio dell'ultimo visto.

### Copiare un programma su un'immagine esistente

```sh
cd system && cargo build --release -p saluto && cd ..
tools/target/release/arctool fs build/arcos.img put \
    system/target/x86_64-unknown-none/release/saluto /apps/saluto.app
```
