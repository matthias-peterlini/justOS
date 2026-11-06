# Architettura di JustOS

## Avvio

```
 firmware ──► JustBoot (BIOS o UEFI) ──► kernel ──► driver di avvio ──► sys: ──► tutti i driver ──► shell
```

### L'immagine del disco

`justtool image` produce un disco con partizioni MBR che si avvia sia da BIOS
sia da UEFI:

| Settori | Contenuto |
|---|---|
| 0 | MBR: codice dello stadio 1, tabella di avvio (da 0x180, firma `ARCB` a 0x190), tabella delle partizioni |
| 1–63 | stadio 2 del loader BIOS |
| 2048– (partizione 2, tipo 0xEF) | partizione di sistema EFI, FAT16: `EFI/BOOT/BOOTX64.EFI`, `JUSTOS/BOOT.CTL` e i due **slot di sistema** `JUSTOS/SLOTA.BIN`, `JUSTOS/SLOTB.BIN` (24 MiB l'uno, `make SLOT_SIZE=…`) |
| partizione 1 (tipo 0xAC) | il volume JustFS "System", che diventa `sys:` |

Uno **slot** contiene una versione del sistema: un settore di intestazione
(versione, posizioni, SHA-256 del resto), il kernel (immagine piatta) e il
pacchetto di avvio. Su un disco nuovo i due slot contengono lo stesso
sistema; `BOOT.CTL` (un settore) dice quale si avvia. I file sono contigui:
il loader BIOS e il kernel li raggiungono per settori, il loader UEFI come
file. La tabella di avvio dell'MBR punta a `BOOT.CTL` e, per il caso in cui
sia illeggibile, al kernel e al pacchetto dello slot A. Vedi
[Aggiornamenti del sistema](#aggiornamenti-del-sistema-ab).

Il **pacchetto di avvio** è un piccolo volume JustFS in sola lettura con
tutti i driver che vengono con il sistema: in `drivers/` quelli marcati
`boot = yes` (dischi, controller USB, tastiere e chiavette USB), in
`system/` gli altri. Il loader lo carica in memoria e il kernel lo monta
come `boot:`. Così il kernel può leggere il disco di sistema con un driver
normale, invece di averne uno incorporato, e una versione del sistema
porta con sé i suoi driver.

### Il CD live

`justtool iso` produce un CD ISO 9660 con un catalogo El Torito per BIOS e
per UEFI (`tools/justtool/src/iso.rs`):

| Settori da 2048 byte | Contenuto |
|---|---|
| 16–22 | descrittori del volume, catalogo di avvio, cartella radice (vuota) |
| 23 | stadio 1 con la sua tabella di avvio (il BIOS lo carica senza emulazione) |
| poi | stadio 2, kernel |
| poi | immagine FAT dell'avvio UEFI: `BOOTX64.EFI`, `KERNEL.BIN`, `BOOTPACK.IMG`, `SYSTEM.IMG` |

`SYSTEM.IMG` è il volume "System" (48 MiB, `make LIVE_SIZE=…`). Il loader
BIOS legge il pacchetto di avvio e il volume di sistema direttamente dai
settori dell'immagine FAT, dove i file sono contigui e allineati a 2048
byte. Entrambi i loader caricano il volume di sistema in memoria e lo
passano nella `BootInfo`; il kernel lo monta come `sys:` (in memoria: le
modifiche si perdono allo spegnimento). Così non serve un driver per i
lettori CD.

### JustBoot per BIOS (`boot/bios`)

- **Stadio 1** (512 byte, nell'MBR o nel primo settore di avvio del CD):
  legge lo stadio 2 con INT 13h esteso. La tabella di avvio dà posizioni e
  dimensioni in settori da 512 byte e dice se il disco è un CD: lì i
  settori sono di 2048 byte e i loader convertono.
- **Stadio 2**, in modo reale e "unreal":
  1. abilita la linea A20;
  2. sceglie lo slot da `BOOT.CTL` (sotto) e carica il suo kernel a 1 MiB,
     il pacchetto di avvio subito dopo e, da un CD live, il volume di
     sistema dopo ancora;
  3. legge la mappa della memoria (E820), copia il font 8×16 del BIOS e
     cerca la tabella RSDP di ACPI;
  4. sceglie con VBE un modo grafico lineare a 32 bit (preferito
     1024×768), altrimenti resta in modo testo 80×25;
  5. costruisce le tabelle delle pagine, passa in modo lungo a 64 bit e
     salta al kernel con `rdi` = indirizzo fisico della `BootInfo`.

### JustBoot per UEFI (`boot/uefi`)

È un'applicazione EFI scritta in Rust senza librerie: le strutture e i
GUID UEFI che usa sono dichiarati a mano in `efi.rs`. Esegue gli stessi
passi del loader BIOS con i servizi del firmware:

1. dalla partizione da cui è stato avviato (Loaded Image + Simple File
   System) sceglie lo slot da `JUSTOS/BOOT.CTL` e lo carica; su un CD live
   legge invece `KERNEL.BIN`, `BOOTPACK.IMG` e `SYSTEM.IMG`;
2. sceglie un modo grafico con il Graphics Output Protocol;
3. prende l'RSDP dalla tabella di configurazione;
4. costruisce le tabelle delle pagine, legge la mappa della memoria e la
   converte nel formato E820, poi chiama `ExitBootServices`;
5. salta al kernel come il loader BIOS.

Il kernel non viene caricato a 1 MiB, ma dove il firmware ha spazio, a un
indirizzo ≡ 1 MiB (mod 2 MiB). Così le pagine da 2 MiB del kernel restano
allineate. La memoria che il loader ha allocato (kernel, pacchetto, tabelle,
mappa) è segnata come riservata. Il firmware non fornisce un font: il kernel
usa il suo (`console/font.rs`).

### La consegna: `BootInfo`

Struttura `#[repr(C)]` definita in `system/kernel/src/boot.rs` (e copiata
in `stage2.asm` e in `boot/uefi`): mappa della memoria, framebuffer
(indirizzo, dimensioni, posizione dei canali di colore), font, posizione e
dimensione del kernel, del pacchetto di avvio e del volume di sistema di un
CD live, RSDP, tipo di loader e lo slot avviato (con i segni "in prova" e
"l'altro non è partito").

### Aggiornamenti del sistema (A/B)

Un aggiornamento di JustOS è un'immagine di slot (`justtool slot`,
`justos-<versione>.slot`). Il kernel (`sysupdate.rs`) la scrive nello slot
che **non** è in esecuzione, la rilegge e ne controlla lo SHA-256, e solo
allora la rende quella che parte, **in prova**. `BOOT.CTL` tiene lo slot
attivo, i tentativi che gli restano e quali slot sono noti per funzionare:

```
BOOT.CTL: "ARCCTL01" · settore di A · settore di B · settori per slot
          slot attivo · tentativi · slot che funzionano (bit) · numero di sequenza
          somma di controllo (i 128 dword sommano a 0)
```

1. Il loader avvia lo slot attivo. Se non è ancora noto per funzionare,
   consuma un tentativo (riscrive `BOOT.CTL`) e lo avvia "in prova"; con i
   tentativi finiti, o con l'intestazione rovinata, avvia l'altro.
2. Il nuovo sistema, se gira per 15 secondi, si dichiara funzionante.
   Un kernel panic durante la prova riavvia la macchina invece di fermarla;
   un sistema in prova che non trova il proprio disco si riavvia da sé.
3. Esauriti i due tentativi, il loader torna allo slot che funzionava; quel
   sistema se ne accorge, torna attivo e lo dice con una notifica.

Il sistema in esecuzione non viene mai scritto: un'interruzione di
corrente a metà aggiornamento lascia le cose come stavano. L'intestazione
dello slot nuovo si scrive per ultima.

Gli aggiornamenti arrivano dall'indice firmato degli aggiornamenti
(`system`: versione, indirizzo, dimensione e SHA-256 dell'immagine), lo
stesso delle app: con gli aggiornamenti automatici si installano da soli e
aspettano un riavvio, altrimenti Info sistema li propone. Info sistema
permette anche di annullare un aggiornamento in attesa e di tornare alla
versione precedente; dal terminale c'è `sysupdate`.

## Memoria

Spazio di indirizzi virtuale (metà alta, uguale in tutti i processi):

| Inizio | Contenuto |
|---|---|
| `0xFFFF_8000_0000_0000` | mappa diretta di tutta la memoria fisica (pagine da 2 MiB) |
| `0xFFFF_9000_0000_0000` | heap del kernel |
| `0xFFFF_A000_0000_0000` | memoria dei dispositivi (MMIO), anche write-combining per il framebuffer |
| `0xFFFF_B000_0000_0000` | moduli dei driver |
| `0xFFFF_C000_0000_0000` | stack dei thread del kernel (64 KiB, con pagina di guardia) |
| `0xFFFF_FFFF_8000_0000` | immagine del kernel |

La metà bassa appartiene al programma in esecuzione.

- **Pagine fisiche**: una bitmap, costruita dalla mappa della memoria.
- **Heap del kernel**: lista libera ordinata per indirizzo, con fusione dei
  blocchi vicini. Cresce mappando nuove pagine.

## Kernel

| Parte | File | Cosa fa |
|---|---|---|
| CPU | `arch/` | GDT con TSS per processore, IDT, PIC 8259 rimappato a 32–47, PIT a 1 kHz, avvio degli altri processori (`smp.rs`) |
| scheduler | `task.rs` | thread del kernel su tutti i processori, round-robin preemptivo (10 ms), code di attesa, sleep |
| sincronizzazione | `sync.rs` | `IrqLock` (spinlock con interruzioni spente), `Mutex` (cede il processore mentre aspetta) |
| console | `console/` | terminale con sequenze ANSI su framebuffer, VGA testo e porta seriale; giornale di sistema; con la grafica attiva è una griglia mostrata dalla finestra Terminale |
| grafica | `gui/` | splash d'avvio, configurazione guidata, scrivania con animazioni, app di sistema, finestre dei programmi, schermata di errore ([gui.md](gui.md)); il disegno è nella libreria `justgfx` |
| Linux | `linux/` | caricatore ELF (statico o con `ld.so`) e chiamate di sistema Linux ([compat.md](compat.md)) |
| dispositivi | `dev/` | registro dei dispositivi, PCI, piattaforma (8042), IRQ condivisi, dischi e partizioni (MBR, GPT), tastiera, interfacce di rete, uscite audio (conversione, volume, WAV), controller video (risoluzioni) |
| driver | `driver/` | gestore dei driver, caricatore ELF dei moduli, tabella `KernelApi` |
| file | `fs/` | volumi, percorsi `volume:/cartella/file`, montaggio automatico |
| rete | `net.rs` | ARP, IPv4, ICMP, UDP, DHCP, DNS |
| programmi | `user.rs` | processi, spazi di indirizzi, caricatore, chiamate di sistema (anche le finestre, `WIN_*`) |
| negozio | `store.rs` | i pacchetti di `sys:/store`: elenco, installazione, rimozione |
| ACPI | `acpi/` | tabelle, spegnimento (`_PTS`, S5), riavvio, pulsante di accensione; l'AML del firmware: GPE, embedded controller, batteria, alimentatore, coperchio, temperature |
| sospensione | `sleep.rs`, `acpi/sleep.rs`, `dev/pm.rs` | sospensione in RAM (S3): dispositivi, processori, firmware e risveglio |
| shell | `shell.rs` | l'interfaccia a riga di comando |

### ACPI e AML

Il firmware descrive il computer con le tabelle ACPI; la DSDT e le SSDT
contengono codice, l'AML, che solo lui sa come parla al suo hardware:
quanto è carica la batteria, se il coperchio è chiuso, quanto è caldo il
processore. JustOS lo esegue con il suo interprete, `justaml`
(`system/libs/justaml`, senza `std`, provato anche sul PC di sviluppo con
`cargo test -p justaml` e con le tabelle di portatili veri). Copre l'AML
che i firmware usano davvero: tipi, pacchetti, campi (anche Index e Bank),
buffer field, metodi con argomenti e variabili locali, `While` con limite
di tempo, mutex ed eventi, `Notify`, `_OSI` (risponde come Windows, come
fa Linux), regioni in memoria, I/O, configurazione PCI, embedded controller
e CMOS.

Nel kernel (`acpi/`) l'interprete vive in un thread suo, `acpi`, con uno
stack da 1 MiB: carica le tabelle, chiama `_REG`, `_STA` e `_INI` dei
dispositivi, prende le porte dell'embedded controller (dalla ECDT o dal
`_CRS` del dispositivo PNP0C09) e abilita i GPE con un metodo `_Lxx`/`_Exx`.
L'interruzione SCI segna quali GPE sono scattati e lo sveglia; lui esegue
i metodi (per l'embedded controller, i `_Qxx` degli eventi in coda) e
raccoglie i `Notify`. Il resto del kernel gli chiede qualcosa con
`aml::with` (una coda di lavori con risposta), così l'AML gira sempre in
un solo posto.

`acpi/power.rs` legge batterie (`_BIX` o `_BIF`, `_BST`), alimentatore
(`_PSR`), coperchio (`_LID`) e zone termiche (`_TMP`, `_CRT`) all'avvio, a
ogni `Notify` e ogni 30 secondi (10 per le temperature). Avvisa al 10% e al
5%, spegne il computer in modo pulito quando la batteria è vuota o una
zona resta sopra la temperatura critica. Lo stato si vede nel dock, nel
centro di controllo, in Info sistema e con `acpi` nel Terminale
(`acpi tree` mostra i dispositivi, `acpi eval` esegue un metodo).

`make test-acpi` prova il pulsante di accensione su PIIX4 e Q35 e un
«portatile»: QEMU con la SSDT di `scripts/acpi/laptop.asl` (batteria,
alimentatore, coperchio, zona termica), di cui scarica la batteria.

### Sospensione

La sospensione in RAM (S3) spegne tutto tranne la memoria; al risveglio
il sistema riparte da dove era. La chiedono il menu della scrivania, il
tasto di sospensione, la chiusura del coperchio o `suspend` nel Terminale
(`suspend 10` si risveglia da sola dopo 10 secondi, con la sveglia del
CMOS; `suspend check` dice se si può). `sleep.rs` fa i passi in ordine, e
se uno fallisce disfa quelli già fatti:

1. controlla che il firmware offra S3 (`\_S3`) e che ogni dispositivo
   con un driver abbia i suoi hook (`dev/pm.rs`, vedi
   [driver.md](driver.md#sospensione));
2. con la grafica attiva mette su la schermata di blocco, se c'è una
   password: è la prima cosa che si vede al risveglio;
3. scrive i file system su disco (se la batteria si scarica, nulla va
   perso), avvisa il firmware (`_TTS`), trattiene l'I/O verso i
   dispositivi, salva la configurazione PCI e ferma i dispositivi, dai
   figli ai genitori;
4. arma i dispositivi che possono svegliare il computer (`_PRW`, `_DSW`),
   chiama `_PTS(3)` e ferma gli altri processori;
5. `acpi/sleep.rs` salva i registri sulla pila, scrive il vettore di
   risveglio nella FACS e SLP_EN nel chipset.

Al risveglio il firmware salta al vettore: è lo stesso trampolino in modo
reale che avvia i processori secondari (`arch/smp.rs`), che ricarica le
tabelle del kernel e torna in `enter` come se SLP_EN fosse appena stato
scritto. Poi, con le interruzioni spente: l'orologio (il tempo passato
dormendo si legge dal CMOS), la configurazione PCI, PIC e APIC, il timer,
la porta seriale, gli eventi del chipset; con le interruzioni accese: MSI
e MSI-X, gli altri processori, il controller PS/2, i dispositivi (dai
genitori ai figli), l'I/O trattenuto, `_WAK(3)`. Infine lo schermo: se era
ancora nel modo del firmware, il driver lo rimette (il BIOS di QEMU lascia
la scheda in modo testo), e l'interfaccia ridisegna tutto.

`make test-acpi` sospende e risveglia la macchina su PIIX4, Q35 e UEFI con
due processori; con `SLEEP=1`, `make test-drivers` e `make test-audio`
provano ogni driver dopo un risveglio; `make test-display` ha i casi
`*-sleep` per gli schermi.

### Dispositivi e driver

Il kernel conosce i bus (PCI e piattaforma), non i dispositivi: tutto il
resto lo fanno i driver. Un driver è un'immagine ELF indipendente dalla
posizione. Il kernel la carica nella regione dei moduli, applica le
rilocazioni e chiama `just_driver_entry` con la tabella dei servizi
(`KernelApi`). In cambio riceve una `DriverInfo` con `probe` e `remove`. I
driver registrano dischi, sorgenti di input, interfacce di rete, uscite
audio e controller video; il kernel fa il resto: partizioni e volumi,
tastiera e mappe dei tasti, IP, conversione dei suoni, ridisegno
dell'interfaccia alla nuova risoluzione. La scelta
e l'installazione automatica sono descritte in [driver.md](driver.md).

I bus che il kernel non conosce li gestiscono i driver. Il driver di un
controller USB enumera i dispositivi collegati e li aggiunge al kernel come
figli del controller (`device_add`); il kernel sceglie per ognuno un
driver, come per un dispositivo PCI. Quando un dispositivo viene
scollegato, il kernel toglie lui e tutto quello che sta dietro (`device_remove`):
chiama `remove` dei driver, toglie dischi e interfacce e smonta i volumi.

Se il disco di sistema è USB, il kernel aspetta che i controller USB
abbiano enumerato i loro dispositivi prima di cercare `sys:`.

Le interruzioni arrivano dalle 16 linee classiche del PIC 8259, condivise
tra più dispositivi, o da MSI e MSI-X, che il kernel programma su un
vettore libero per dispositivo e riceve tramite l'APIC locale.

### Più processori

Il firmware avvia solo il primo processore. Il kernel trova gli altri
nella tabella ACPI MADT e li sveglia con INIT e SIPI: partono in modo
reale a `0x8000`, dove un piccolo trampolino passa direttamente al modo a
64 bit e salta nel kernel. Ognuno ha la sua GDT con TSS, il suo timer
(l'APIC locale, a 1 kHz) e un thread di inattività; i suoi dati (la pila
del kernel per `syscall`, il numero del processore) si trovano tramite la
base di GS.

La coda dei thread pronti è una sola, e ogni processore ci prende il
prossimo thread. I thread dei programmi restano sul primo processore, così
il loro spazio di indirizzi non è mai attivo altrove. Quando il kernel
toglie una pagina della sua metà (la pila di un thread finito), chiede agli
altri processori di svuotare il TLB con un'interruzione e aspetta che lo
abbiano fatto prima di riusare la memoria.

### File system: JustFS

```
blocco 0             superblocco (etichetta, dimensioni, posizione delle aree)
bitmap               un bit per blocco
tabella dei nodi     nodi da 128 byte (il nodo 1 è la radice)
dati                 contenuto di file e cartelle
```

- Blocchi da 4 KiB.
- Ogni nodo descrive il contenuto con fino a 11 extent nel nodo, più 512 in
  un blocco indiretto.
- Le cartelle sono elenchi di voci da 64 byte; i nomi arrivano a 56 byte e
  non distinguono maiuscole e minuscole.
- Un collegamento simbolico è un nodo il cui contenuto è il percorso a cui
  punta: la ricerca di un percorso lo segue (fino a 40 collegamenti), da
  radice del volume se il percorso è assoluto, dalla sua cartella se è
  relativo.
- `check` verifica la coerenza tra bitmap, nodi e cartelle.

La stessa libreria (`system/libs/justfs`) serve al kernel e ad `justtool`,
che così crea e modifica i volumi dal computer di sviluppo.

All'avvio ogni partizione JustFS viene montata. La prima con etichetta
"System" diventa `sys:`, le altre prendono il nome dalla loro etichetta.

### Programmi

Ogni processo ha la sua tabella delle pagine: la metà alta è condivisa con
il kernel, la metà bassa è sua. Il programma gira in ring 3 e chiama il
kernel con `int 0x80`. Il kernel controlla ogni puntatore che riceve. Un
errore del programma (page fault, istruzione non valida...) termina solo
quel processo e ne libera la memoria. L'elenco delle chiamate è in
[programmi.md](programmi.md).

Ogni thread porta con sé, oltre ai registri, le basi dei segmenti FS e GS e
lo stato dell'unità SSE (salvato e ripristinato a ogni cambio di contesto con
`fxsave`/`fxrstor`); FS e GS servono alla memoria locale del thread, che i
programmi C e C++ usano di continuo. Il kernel abilita SSE all'avvio
(`arch::cpu::enable_sse`), così le istruzioni SSE dei programmi comuni non
sollevano un'eccezione.

### Programmi Linux e Windows

Oltre ai programmi nativi, il kernel riconosce altri due tipi di eseguibile
dal loro contenuto. Un ELF che non è un `.app` è un **programma Linux**:
`linux/` lo carica (ET_EXEC o PIE, con stack iniziale e vettore ausiliario;
se chiede un caricatore dinamico, carica anche quello e parte da lì) e serve le chiamate di sistema Linux che arrivano sull'istruzione
`syscall` (`arch/syscall.rs`), traducendole nelle operazioni di JustOS. Un file
che inizia con `MZ` è un **programma Windows**: il kernel avvia `winrun`, un
normale programma di JustOS che carica il `.exe`, risolve le importazioni verso
un `kernel32`/`msvcrt` scritti da zero e salta al punto d'ingresso. I
dettagli, e cosa è gestito, sono in [compat.md](compat.md).
