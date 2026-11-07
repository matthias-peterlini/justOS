# JustScript su justOS

JustScript è il linguaggio applicativo di justOS: indentazione al posto
delle graffe, struct, enum con `match`, `Option`/`Result` con `?`, closure,
moduli, una libreria standard e un'interfaccia dichiarativa (`view`). È nato
come ArcScript ([repository originale](https://github.com/matthias-peterlini/ArcScript)):
su justOS girano il suo compilatore e la sua macchina virtuale, portati in
Rust `no_std`. L'IDE è **JustStudio**, preinstallato nell'interfaccia
grafica: lì le `view` si vedono e si usano dal vivo (vedi sotto); dal
terminale `just` e `justc` le stampano come testo.

Riferimenti del linguaggio, dal repository originale:
[LANGUAGE.md](justscript/LANGUAGE.md) (il linguaggio),
[STDLIB.md](justscript/STDLIB.md) (la libreria standard) e
[JPK_FORMAT.md](justscript/JPK_FORMAT.md) (il formato `.jpk`).

## Nomi e formati

| | |
|---|---|
| sorgenti | `.jt` |
| pacchetti (programmi compilati) | `.jpk` (*Just Package*): intestazione `4A 50 4B 00` (`"JPK\0"`), vedi [JPK_FORMAT.md](justscript/JPK_FORMAT.md) |
| programmi per Windows | `.exe` autonomi, con il runtime dentro: `justc build -t windows` |
| progetto | una cartella con un `Just.toml` |
| compilatore | `justc` (`justc run main.jt`, `justc build`…) |
| runner | `just` (`just main.jt`, `just app.jpk`): il programma con cui il sistema apre i `.jt` e i `.jpk` |
| IDE | JustStudio |
| macchina virtuale | JustVM (`system/libs/just-vm`) |

I nomi di prima (`.as`, `.arc`, `Arc.toml`, `arcc`) non valgono più: un
sorgente `.as` si rinomina in `.jt`, un `Arc.toml` in `Just.toml`, e un
pacchetto `.arc` si ricompila con `justc build`.

## Usarlo

```
hello.jt                       un sorgente si avvia come un programma
just hello.jt [argomenti]      lo stesso, con il runner
notes add "Comprare il latte"  un programma JustScript installato in sys:/apps
library/build/release/library.jpk   un pacchetto compilato, idem
just --info <file.jpk>         il manifest e il contenuto di un pacchetto
justc run <file|cartella> [argomenti]
justc build <file|cartella> [-r] [-t jpk|windows] [-o uscita]
justc check <file|cartella>
justc disasm <file.jt|file.jpk>
justc info <file.jpk|file.exe>
justc new <cartella>
```

`justc` ha gli stessi comandi del compilatore del desktop; `justc info` è
anche `just --info`. Una cartella è un progetto con un `Just.toml`; senza
percorso vale la cartella corrente. `justc build` scrive il pacchetto in
`<progetto>/build/debug/<nome>.jpk` (con `-r` in `build/release/`), o
accanto al sorgente per un file singolo. Gli argomenti dopo il file (o dopo
`--`) arrivano al programma come `os.args()`.

## Programmi per Windows

`justc build -t windows` (anche `-t exe`) scrive, al posto del `.jpk`, un
`<nome>.exe` che su Windows (10 o successivi, 64 bit) gira da solo, senza
installare niente: è il runtime `just-stub` con il pacchetto in coda, nel
formato descritto in [JPK_FORMAT.md](justscript/JPK_FORMAT.md#standalone-executables)
(trailer `JPKBNDL\0`). Il runtime è in `sys:/system/justc/just-stub.exe`;
il sorgente è in `tools/just-stub`, il formato in `system/libs/jpk-bundler`.
In JustStudio lo sceglie il selettore `.jpk | .exe` accanto a **Compila**.

Su Windows il programma usa la console, i file con i percorsi di Windows,
l'orologio, le variabili d'ambiente e `os.run` (che lì cattura `stdout` e
`stderr`); `os.name` è `"windows"`. Una `view` è mostrata come testo, con
i pulsanti, le caselle e i campi numerati: si usano scrivendone il numero
(Invio a vuoto chiude). Le finestre vere, come in JustStudio, non ci sono
ancora. `justc info programma.exe` mostra il pacchetto dentro un `.exe`;
`just-stub app.jpk` esegue un pacchetto con il runtime da solo.

Gli `.exe` non sono firmati: SmartScreen li segnala e, dove è attivo,
Smart App Control può bloccarli ("An Application Control policy has blocked
this file"). Per distribuirli vanno firmati con un certificato di firma del
codice (`signtool sign` o `osslsigncode`) dopo `justc build`: la firma
Authenticode va in coda al file, dopo il pacchetto, e il runtime la salta
(lo verifica il test `signed_after_bundling` di `jpk-bundler`).

Il sistema apre i file JustScript con `just`: la shell avvia i `.jpk` e i
`.jt` sia scritti per percorso sia per nome (`notes` cerca
`sys:/apps/notes.app`, poi `notes.jpk`, poi `notes.jt`), e lo stesso vale
per i programmi avviati da altri programmi. In File un doppio clic su un
`.jpk` lo esegue nel Terminale, su un `.jt` lo apre in JustStudio.
Nell'elenco dei processi il programma ha il nome dello script.

Gli esempi del repository originale sono in `sys:/home/justscript`:
`hello.jt`, `notes.jt`, `todo.jt`, `elements.jt`, il progetto `counter` e
il progetto su più file `library`. Il taccuino `notes` è installato anche
come programma di sistema (`sys:/apps/notes.jt`).

## Cosa fa la libreria standard su justOS

Tutto ciò che un programma usa fuori dalla macchina virtuale passa per
l'interfaccia `Host` di `just-vm`. Su justOS la implementano `just` e
`justc` (`system/apps/justc/src/host.rs`):

| Modulo | Su justOS |
|---|---|
| `print`, `read_line` | la console; `read_line` restituisce `None` con Esc |
| `fs` | i volumi montati, con percorsi come `sys:/home/nota.txt`; quelli relativi partono dalla cartella corrente |
| `path` | `/` separa i nomi; `sys:` (un volume) rende assoluto un percorso |
| `time` | l'orologio del computer (`time.now()` conta dal 1970 come altrove; justOS non conosce i fusi orari e legge l'orologio così com'è) |
| `os.args`, `os.cwd`, `os.exit` | come altrove |
| `os.name` | `"justos"` |
| `os.env` | sempre `None`: justOS non ha variabili d'ambiente |
| `os.run` | avvia un programma e ne aspetta la fine; `code` è il suo codice d'uscita, `stdout` e `stderr` restano vuoti perché l'uscita va direttamente sulla console |
| `http` | non ancora: restituisce `Err` (lo stack di rete non ha ancora TCP) |
| `os.notify` | non ancora: restituisce `Err` |
| `math`, `random`, `json`, stringhe e liste | come altrove |

Un programma con una `view` esegue il suo codice, poi `just` (o
`justc run`) stampa l'interfaccia come testo (lo stesso formato
dell'anteprima testuale del compilatore).

## JustStudio

L'IDE `just-studio` del repository originale è scritto con egui e wgpu, che
su justOS non ci sono: JustStudio è la stessa idea rifatta come app nativa
dell'interfaccia grafica (`system/kernel/src/gui/apps/juststudio.rs`), con
`justc-core` e `just-vm` collegati direttamente, senza un programma a parte.

- **Esplora**: i progetti (cartelle con `Just.toml`) e i file `.jt` di
  `sys:/home/justscript`; il progetto aperto mostra i suoi sorgenti. *Nuovo
  file* (Ctrl+N) crea un `.jt` di esempio con una `view`.
- **Editor**: numeri di riga, colori dall'evidenziatore di `justc-core`
  (`highlight`), Tab di quattro spazi, Invio che mantiene il rientro e lo
  aumenta dopo `fn`, `if`, `view`, `window`, `column`...; Ctrl+S salva, e
  alla chiusura si salva da solo.
- **Errori mentre si scrive**: tre decimi di secondo dopo l'ultimo tasto il
  progetto viene ricompilato (con il testo non ancora salvato, anche per i
  moduli importati); gli errori del file aperto sono sottolineati in rosso,
  e tutti, con gli avvisi, sono nel pannello **Problemi**.
- **Anteprima dal vivo**: se la compilazione riesce, il programma riparte in
  una macchina virtuale nella finestra (al massimo 3 milioni di istruzioni
  per chiamata, così un ciclo infinito non blocca il desktop) e la sua
  `view` è disegnata con i colori Material calcolati dall'`accent` e dal
  `theme` della finestra: etichette, pulsanti nei vari stili, card, righe,
  colonne, griglie, split, campi di testo, caselle, interruttori, cursori,
  barre di avanzamento, accordion. Clic, digitazione e trascinamenti vanno
  al programma (`UiEvent`), che ridisegna la `view`. Con errori nel codice
  resta l'ultima anteprima buona, segnalata.
- **Console**: l'output di `print()` dell'ultima esecuzione e gli errori di
  esecuzione.
- **Esegui** (F5 o Ctrl+R) salva e riparte da capo; **Compila** (Ctrl+B)
  apre il Terminale con `justc build`, che produce il `.jpk`, o con
  `justc build -t windows` se il selettore accanto è su `.exe`; **Nel
  terminale** con `justc run`, per i programmi che leggono dalla tastiera.

Le icone Material dei pulsanti non hanno ancora un font: le più comuni
(`add`, `remove`, `close`...) sono disegnate con un carattere simile, le
altre omesse. Le immagini (`image`) sono un riquadro con il nome del file.

## Com'è fatto il port

Il codice viene dal repository originale (commit `02dab18`, versione
0.5.0) ed è cambiato il meno possibile, così da poterlo riallineare:

- `system/libs/justc-core`: lexer, parser, compilatore, bytecode, formato
  `.jpk` (`jpk.rs`) e diagnostica. Gli eseguibili autonomi (`bundle.rs`
  dell'originale) sono in `system/libs/jpk-bundler`, `no_std` come il
  resto, con il runtime per Windows in `tools/just-stub`.
- `system/libs/just-vm`: la macchina virtuale, i valori, le funzioni
  predefinite, la libreria standard e l'albero dell'interfaccia (`ui.rs`).
  Sono rimasti fuori il renderer (egui), i temi e il launcher del desktop.
- `system/apps/justc`: i due programmi `justc` e `just` (i comandi sono in
  `cli.rs`, in comune), con l'`Host` di justOS e la lettura dei progetti
  (`Just.toml`).

Le differenze rispetto all'originale:

- `#![no_std]` con `alloc`: i percorsi `std::` sono diventati `core::` e
  `alloc::`, `HashMap`/`HashSet` sono diventati `BTreeMap`/`BTreeSet`.
- `justc-core/src/deflate.rs` sostituisce il crate `miniz_oxide`: legge ogni
  tipo di blocco DEFLATE e comprime con LZ77 e codici fissi. I pacchetti
  restano compatibili nei due sensi con quelli fatti dal compilatore del
  desktop.
- `justc-core/src/float.rs` sostituisce le funzioni `f64` che Rust tiene in
  `std` (`sqrt`, `sin`, `exp`, `powf`…), con un trait che usa gli stessi
  nomi di metodo. `sqrt` e gli arrotondamenti sono corretti al bit; le altre
  differiscono dalla libreria C di qualche unità nell'ultima cifra al
  massimo.
- Il trait `Host` di `just-vm` è cresciuto: oltre a `print` e `read_line`
  ha file, orologio, ambiente, programmi, rete e notifiche, con valori
  predefiniti che rispondono "non disponibile". Su un sistema con `std` si
  implementa in poche righe (lo fa il test `tests/common/mod.rs`).

## Test

- `make check` esegue anche i test di `justc-core`, `just-vm` e
  `jpk-bundler` (sul computer di sviluppo): sono quelli del repository
  originale, più quelli di `deflate` e `float`, che confrontano le
  funzioni con quelle di `std`, e quelli del formato degli `.exe`
  (anche firmati).
- `make test-justscript` avvia justOS in QEMU, copia nell'immagine i
  programmi di `scripts/justscript/` ed esegue con `just` e `justc` esempi,
  progetti, pacchetti `.jpk`, file, errori di compilazione e di esecuzione,
  `read_line` e la matematica (confrontata con i valori della libreria C),
  controllandone l'uscita. Poi legge dal disco il `.jpk` e l'`.exe` che
  `justc` ha scritto, ne controlla le intestazioni ed esegue il pacchetto
  dentro l'`.exe` con `just-stub`. Lo esegue anche il workflow *Build* di GitHub
  Actions a ogni push.
