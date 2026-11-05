# ArcScript su JustOS

[ArcScript](https://github.com/matthias-peterlini/ArcScript) è il
linguaggio applicativo di JustOS: indentazione al posto delle graffe, struct,
enum con `match`, `Option`/`Result` con `?`, closure, moduli, una libreria
standard e un'interfaccia dichiarativa (`view`). Su JustOS gira il compilatore
e la macchina virtuale del repository ArcScript, portati in Rust `no_std`.
L'IDE è **ArcStudio**, preinstallato nell'interfaccia grafica: lì le `view`
si vedono e si usano dal vivo (vedi sotto); `arcc` dal terminale le stampa
come testo.

Riferimenti del linguaggio, copiati dal repository ArcScript:
[LANGUAGE.md](arcscript/LANGUAGE.md) (il linguaggio),
[STDLIB.md](arcscript/STDLIB.md) (la libreria standard) e
[ARC_FORMAT.md](arcscript/ARC_FORMAT.md) (il formato `.arc`).

## Usarlo

```
hello.as                       un sorgente si avvia come un programma
notes add "Comprare il latte"  un programma ArcScript installato in sys:/apps
library/build/release/library.arc   un pacchetto compilato, idem
arcc run <file|cartella> [argomenti]
arcc build <file|cartella> [-r] [-o uscita.arc]
arcc check <file|cartella>
arcc disasm <file.as|file.arc>
arcc info <file.arc>
arcc new <cartella>
```

`arcc` ha gli stessi comandi dell'`arcc` del desktop (più `info`, che lì è
`arc-runner --info`). Una cartella è un progetto con un `Arc.toml`; senza
percorso vale la cartella corrente. Gli argomenti dopo il file (o dopo
`--`) arrivano al programma come `os.args()`.

La shell avvia i file `.arc` e `.as` attraverso `arcc`, sia scritti per
percorso sia per nome: `notes` cerca `sys:/apps/notes.app`, poi
`notes.arc`, poi `notes.as`. Lo stesso vale per i programmi avviati da altri
programmi. Nell'elenco dei processi il programma ha il nome dello script.

Gli esempi del repository ArcScript sono in `sys:/home/arcscript`:
`hello.as`, `notes.as`, `todo.as`, `elements.as`, il progetto `counter` e
il progetto su più file `library`. Il taccuino `notes` è installato anche
come programma di sistema (`sys:/apps/notes.as`).

## Cosa fa la libreria standard su JustOS

Tutto ciò che un programma usa fuori dalla macchina virtuale passa per
l'interfaccia `Host` di `arc-vm`. Su JustOS la implementa `arcc`
(`system/apps/arcc/src/host.rs`):

| Modulo | Su JustOS |
|---|---|
| `print`, `read_line` | la console; `read_line` restituisce `None` con Esc |
| `fs` | i volumi montati, con percorsi come `sys:/home/nota.txt`; quelli relativi partono dalla cartella corrente |
| `path` | `/` separa i nomi; `sys:` (un volume) rende assoluto un percorso |
| `time` | l'orologio del computer (`time.now()` conta dal 1970 come altrove; JustOS non conosce i fusi orari e legge l'orologio così com'è) |
| `os.args`, `os.cwd`, `os.exit` | come altrove |
| `os.name` | `"justos"` |
| `os.env` | sempre `None`: JustOS non ha variabili d'ambiente |
| `os.run` | avvia un programma e ne aspetta la fine; `code` è il suo codice d'uscita, `stdout` e `stderr` restano vuoti perché l'uscita va direttamente sulla console |
| `http` | non ancora: restituisce `Err` (lo stack di rete non ha ancora TCP) |
| `os.notify` | non ancora: restituisce `Err` |
| `math`, `random`, `json`, stringhe e liste | come altrove |

Un programma con una `view` esegue il suo codice, poi `arcc` stampa
l'interfaccia come testo (lo stesso formato dell'anteprima testuale di
ArcScript).

## ArcStudio

L'`arc-studio-ide` del repository ArcScript è scritto con egui e wgpu, che
su JustOS non ci sono: ArcStudio è la stessa idea rifatta come app nativa
dell'interfaccia grafica (`system/kernel/src/gui/apps/arcstudio.rs`), con
`arcc-core` e `arc-vm` collegati direttamente, senza un programma a parte.

- **Esplora**: i progetti (cartelle con `Arc.toml`) e i file `.as` di
  `sys:/home/arcscript`; il progetto aperto mostra i suoi sorgenti. *Nuovo
  file* (Ctrl+N) crea un `.as` di esempio con una `view`.
- **Editor**: numeri di riga, colori dall'evidenziatore di `arcc-core`
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
  apre il Terminale con `arcc build`; **Nel terminale** con `arcc run`, per
  i programmi che leggono dalla tastiera.

Le icone Material dei pulsanti non hanno ancora un font: le più comuni
(`add`, `remove`, `close`...) sono disegnate con un carattere simile, le
altre omesse. Le immagini (`image`) sono un riquadro con il nome del file.

## Com'è fatto il port

Il codice viene dal repository ArcScript (commit `02dab18`, versione
0.5.0) ed è cambiato il meno possibile, così da poterlo riallineare:

- `system/libs/arcc-core`: lexer, parser, compilatore, bytecode, formato
  `.arc` e diagnostica. Sono rimasti fuori solo gli eseguibili autonomi
  (`bundle.rs`), che su JustOS non servono.
- `system/libs/arc-vm`: la macchina virtuale, i valori, le funzioni
  predefinite, la libreria standard e l'albero dell'interfaccia (`ui.rs`).
  Sono rimasti fuori il renderer (egui), i temi e il launcher del desktop.
- `system/apps/arcc`: il programma `arcc` di JustOS, con l'`Host` di JustOS e
  la lettura dei progetti (`Arc.toml`).

Le differenze rispetto all'originale:

- `#![no_std]` con `alloc`: i percorsi `std::` sono diventati `core::` e
  `alloc::`, `HashMap`/`HashSet` sono diventati `BTreeMap`/`BTreeSet`.
- `arcc-core/src/deflate.rs` sostituisce il crate `miniz_oxide`: legge ogni
  tipo di blocco DEFLATE e comprime con LZ77 e codici fissi. I pacchetti
  restano compatibili nei due sensi con quelli fatti da ArcScript sul
  desktop.
- `arcc-core/src/float.rs` sostituisce le funzioni `f64` che Rust tiene in
  `std` (`sqrt`, `sin`, `exp`, `powf`…), con un trait che usa gli stessi
  nomi di metodo. `sqrt` e gli arrotondamenti sono corretti al bit; le altre
  differiscono dalla libreria C di qualche unità nell'ultima cifra al
  massimo.
- Il trait `Host` di `arc-vm` è cresciuto: oltre a `print` e `read_line`
  ha file, orologio, ambiente, programmi, rete e notifiche, con valori
  predefiniti che rispondono "non disponibile". Su un sistema con `std` si
  implementa in poche righe (lo fa il test `tests/common/mod.rs`).

## Test

- `make check` esegue anche i test di `arcc-core` e `arc-vm` (sul computer
  di sviluppo): sono quelli del repository ArcScript, più quelli di
  `deflate` e `float`, che confrontano le funzioni con quelle di `std`.
- `make test-arcscript` avvia JustOS in QEMU, copia nell'immagine i
  programmi di `scripts/arcscript/` ed esegue esempi, progetti, pacchetti,
  file, errori di compilazione e di esecuzione, `read_line` e la
  matematica (confrontata con i valori della libreria C), controllandone
  l'uscita.
