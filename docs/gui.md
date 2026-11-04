# L'interfaccia grafica

Quando il loader trova uno schermo grafico di almeno 800x600, arcOS si avvia
in modalità grafica. Sullo schermo non compare mai testo tecnico: il kernel
scrive i suoi messaggi solo sulla porta seriale e nel giornale di sistema, e
le righe di comando si vedono soltanto aprendo il Terminale. Anche un errore
fatale mostra una schermata semplice («arcOS si è dovuto fermare»), con i
dettagli solo sulla porta seriale.

## Avvio

1. **Splash**: logo, nome e una barra che avanza mentre il kernel avvia le sue
   parti (memoria, dischi, driver, rete) e carica icone e sfondi.
2. **Primo avvio**: una configurazione guidata in cinque passi: *Benvenuto*
   (pulsante **INIZIA**), *Come ti chiami?*, *Scegli la tastiera*, *Scegli lo
   sfondo*, *Tutto pronto*. Si usa con il mouse o con la tastiera (Invio per
   andare avanti, Esc per tornare indietro). Alla fine crea le cartelle della
   home (Documenti, Scaricati, Immagini, Musica, Video, Scrivania, Progetti).
3. **Avvii successivi**: dopo lo splash compare direttamente la scrivania.

Le scelte sono in `sys:/system/settings`; *Impostazioni → Rifai la
configurazione iniziale* la ripropone al prossimo avvio. Il CD live non
conserva nulla, quindi mostra la configurazione ogni volta.

## La scrivania

Presa a modello da macOS:

- **Barra in alto**: il logo (menu con *Info su arcOS*, *Impostazioni di
  sistema*, *Negozio*, *Monitor attività*, *Schermata*, *Blocca schermo*,
  *Sospendi* quando il computer può, *Riavvia*, *Spegni*), il nome
  dell'app in primo piano, ora e data al centro, il **centro di controllo**
  (volume con un cursore, stato della rete) e il pulsante di spegnimento.
  Sui portatili la barra mostra anche la carica della batteria (con il
  fulmine quando è in carica) e il centro di controllo quanto manca.
- **Widget**: saluto con l'orologio, calendario del mese, stato del sistema.
- **Dock**: File, Applicazioni, Editor di testo, Note, Calendario, Foto,
  Musica, Calcolatrice, Negozio, Impostazioni e, dopo un separatore, le app
  aperte che non ci sono già e il Cestino. Le icone **si ingrandiscono** sotto
  il puntatore, quella di un'app che parte **rimbalza**, un puntino segna le
  app aperte; un clic su un'app aperta la porta davanti (o la riapre se è
  ridotta).
- **Applicazioni** (come Launchpad, dal dock o con Ctrl+Spazio): tutte le app,
  comprese quelle installate dal Negozio, con una ricerca; si scrive e Invio
  apre la prima. La ricerca è anche quella di **Spotlight**: sopra le app
  compaiono i **calcoli** (`12*3+4` → `= 40`, Invio lo copia), le
  **impostazioni** per parola («sfondo», «volume», «password», «processore»...)
  e i **file e le cartelle** della home per nome, che si aprono con l'app
  giusta. La home viene letta una volta sola all'apertura della ricerca.
- **Notifiche** in alto a destra: un'app installata, un timer scaduto, un
  file salvato, un programma che si è chiuso per un errore. Spariscono da sole
  o con un clic.
- **Tasto destro** sulla scrivania: cambiare sfondo, impostazioni dello
  schermo, informazioni.
- **Finestre**: barra del titolo separata dal corpo, con i pulsanti riduci,
  ingrandisci e chiudi; si spostano trascinando la barra (doppio clic per
  ingrandire), molte si ridimensionano dall'angolo in basso a destra.
  **Alt+Tab** passa alla finestra successiva, **Ctrl+Q** chiude quella in
  primo piano.
- **Snap**, come in Windows: una finestra trascinata sul bordo sinistro o
  destro occupa quella metà dello schermo, in un angolo un quarto, sul bordo
  in alto tutto lo schermo; mentre si trascina un vetro mostra dove andrà.
  Da tastiera (finestre ridimensionabili) **Ctrl+Alt+←/→** per le metà,
  **Ctrl+Alt+U/I/J/K** per i quarti, **Ctrl+Alt+↑** ingrandisce e
  **Ctrl+Alt+↓** ripristina. Trascinata via, la finestra torna alla sua
  misura di prima.
- **Desktop virtuali e vista d'insieme**: **F3** (o **logo+Tab**, o
  *Desktop e finestre* nel menu del logo) mostra in alto i desktop e sotto
  le finestre di quello attuale affiancate; un clic su una finestra la porta
  davanti, trascinata su un desktop ci si sposta, trascinata sul **+** ne
  crea uno nuovo (fino a sei); la × su un desktop lo toglie e le sue
  finestre passano a quello prima. **Ctrl+Alt+PagSu/PagGiù** (o
  **logo+Ctrl+←/→**) passano al desktop accanto, **Ctrl+Alt+Maiusc+←/→**
  ci portano la finestra in primo piano (oltre l'ultimo ne crea uno). Con
  più desktop, dei pallini dopo il logo dicono dove si è; il dock e Alt+Tab
  portano al desktop della finestra scelta.
- **Trascinare i file**: da File si trascinano uno o più file (Ctrl+clic e
  Maiusc+clic selezionano, Ctrl+A tutto) su una cartella, su una risorsa a
  sinistra, su una scheda o su un'altra finestra di File. Sullo stesso
  volume si spostano, tra volumi diversi si copiano; tenendo **Ctrl** (o
  Alt) si copia sempre e compare un **+** verde, con **Maiusc** si sposta
  sempre. Rilasciati sul Cestino del dock ci finiscono dentro; sull'icona di
  un'app (Editor, Foto, Musica, ArcStudio, File) si aprono lì; sul
  Terminale ne scrivono il percorso; nell'Editor si aprono. Le app del
  Negozio che lo chiedono (Disegno) ricevono i file, solo in lettura.
- **Schermata**: il tasto **Stamp** (o *Schermata* nel menu del logo) salva
  lo schermo in `sys:/home/Immagini` come `Schermata <data> alle <ora>.qoi`,
  che Foto apre subito.
- **Blocco**: **logo+L** o *Blocca schermo* nel menu del logo coprono tutto
  con l'ora sullo sfondo sfocato. Con una password (*Impostazioni →
  Password*) arcOS si avvia bloccato, si blocca da solo dopo 2, 5 o 15
  minuti senza usarlo e prima di ogni sospensione, così al risveglio chiede
  la password; senza password basta Invio. La password non è salvata
  in chiaro: nelle impostazioni c'è un sale casuale e 20 000 passaggi di
  SHA-256.
- **Aggiornamenti**: con la rete, all'avvio e una volta al giorno arcOS
  legge l'indice firmato degli aggiornamenti. Una versione nuova si
  scarica e si scrive nell'altro slot di sistema; con gli aggiornamenti
  automatici succede da sola, poi una notifica chiede di riavviare. In
  *Info sistema* il pulsante passa da *Cerca aggiornamenti* a *Installa*,
  *Scarico…*, *Installo…* e *Riavvia*. Accanto ci sono *Annulla* e *Torna a
  <versione precedente>*. Se la versione nuova non parte, arcOS torna da
  solo alla precedente e lo dice. Una release su GitHub che l'indice non ha
  ancora è solo annunciata.

### Le animazioni

Tutto ciò che si muove segue il tempo, non i fotogrammi: su un computer lento
si vedono meno fotogrammi ma l'animazione dura uguale.

| Cosa | Animazione |
|---|---|
| finestra che si apre | cresce dal 86% e compare in dissolvenza |
| finestra che si chiude | si rimpicciolisce e svanisce |
| riduci / riapri | la finestra vola nella sua icona del dock e ne esce |
| ingrandisci / ripristina | la finestra si allarga fino allo schermo e torna |
| Applicazioni | lo sfondo si sfoca e scurisce, le icone arrivano con uno zoom |
| dock | ingrandimento morbido delle icone, rimbalzo all'avvio di un'app |
| menu e centro di controllo | si aprono dall'alto in un attimo |
| notifiche | scivolano dentro da destra e poi via |
| vista d'insieme | le finestre scivolano dal loro posto alla griglia, i desktop scendono dall'alto |
| cambio di desktop | il nome del desktop e i suoi pallini al centro, per un attimo |
| spegnimento | lo schermo sfuma nel nero con il logo |

Una finestra in movimento viene disegnata una volta in un'immagine a parte
(con la sua trasparenza) e poi solo spostata, scalata e sfumata: l'app non
si ridisegna a ogni fotogramma.

## Le app di sistema

| App | Cosa fa |
|---|---|
| ticly Do | il workspace di ticly: panoramica, attività, note e calendario (mese, settimana, giorno) sugli stessi file di Promemoria, Note e Calendario; profilo dell'account |
| ticly AI | l'interfaccia di ai.ticly.it con le chat salvate in `sys:/home/.ticly/ai`; per ora nessun modello risponde (il collegamento al servizio ticly arriverà) |
| ticly browser | il browser di ticly: schede, barra degli indirizzi (indirizzi o ricerche DuckDuckGo), avanti/indietro, ricarica, pagina iniziale ticly, trova nella pagina (Ctrl+F), moduli, `<select>`, cookie; i file non visualizzabili vanno in `Scaricati` |
| File | sfoglia i volumi: risorse a sinistra, cartelle e file a destra; doppio clic apre una cartella o il file con l'app giusta; **schede** (Ctrl+T o il **+**, Ctrl+W chiude, Ctrl+Tab passa alla successiva, *Apri in una nuova scheda* sulle cartelle); tasto destro per aprire, rinominare (anche F2), spostare nel Cestino (Canc), creare una cartella; nel Cestino eliminare o svuotare; **selezione multipla** (Ctrl+clic, Maiusc+clic, Ctrl+A) e **trascinamento** tra cartelle, schede e finestre |
| Editor di testo | file di testo con selezione, copia e incolla, scorrimento; Nuovo, Apri, Salva (Ctrl+N, Ctrl+O, Ctrl+S) con la scelta del file; alla chiusura salva da solo |
| ArcStudio | l'IDE di ArcScript, preinstallato: esplora i progetti di `sys:/home/arcscript`, editor con numeri di riga, colori della sintassi, rientro automatico ed errori sottolineati mentre si scrive; **anteprima dal vivo** della `view` (il programma gira nella finestra e risponde a pulsanti, interruttori, cursori e campi), console con l'output di `print()`, pannello Problemi (un clic porta alla riga); Esegui (F5), Compila in `.arc` (Ctrl+B) e Nel terminale passano da `arcc`. I file `.as` si aprono qui |
| Note | note in `sys:/home/Note`, la prima riga è il titolo, ricerca, si salvano mentre si scrive |
| Promemoria | cose da fare, spuntate con un clic (`sys:/home/Promemoria.txt`) |
| Calendario | un mese alla volta con le feste italiane (anche Pasqua e Pasquetta) e i propri eventi («9:30 Riunione»), salvati in `sys:/home/Calendario.txt` |
| Orologio | l'ora qui e in altre città, cronometro con i giri, timer che suona e avvisa |
| Calcolatrice | come quella di macOS, anche da tastiera; calcoli in virgola fissa con dodici decimali |
| Foto | le immagini di `Immagini` e gli sfondi, in miniatura e a tutto schermo |
| Musica | i file WAV di `Musica` e i suoni di sistema: riproduzione, pausa, brano precedente e successivo, barra di avanzamento, volume; tre brani di esempio |
| Anteprima | un'immagine sola, aperta da File |
| Negozio | le app di `sys:/store` per categoria, con la loro pagina; installa, apre, rimuove |
| Impostazioni | nome, tastiera, sfondo, risoluzione dello schermo, volume e suono all'avvio, configurazione iniziale |
| Monitor attività | i programmi in esecuzione, il processore e la memoria nell'ultimo minuto; un programma si può terminare |
| Info sistema | panoramica, dispositivi con i loro driver, dischi con lo spazio libero |
| Terminale | la shell di arcOS: l'unico posto dove si vedono le righe di comando |

L'account ticly per ora vive solo su questo computer: «Crea un account»
(nome, e-mail, password) lo salva in `sys:/system/ticly/accounts.json`, con
la password come HMAC-SHA-256 ripetuto 4096 volte con un sale casuale; chi
ha fatto l'accesso è in `sys:/system/ticly/session`. Nessuna di queste app
usa la rete.

### Il motore web

Le pagine del ticly browser le disegna `arcweb` (`system/libs/arcweb`),
scritto da zero come il resto: tokenizer e albero HTML con le regole di
chiusura implicita, CSS con selettori, media query, `@import`, `@supports`,
annidamento, variabili, `calc()`/`min()`/`max()`/`clamp()`; layout a blocchi
e in linea, float, flexbox, grid, tabelle, liste, campi dei moduli; immagini
PNG, JPEG (anche progressive) e GIF.

JavaScript lo esegue `arcjs` (`system/libs/arcjs`), un interprete ad albero
scritto da zero in Rust `no_std`: ES2020 circa (classi, closure,
destrutturazione, spread, template, generatori, `async`/`await`, Promise,
Map/Set, espressioni regolari, JSON, Date, Intl essenziale). `arcweb`
(`script.rs`) gli dà la pagina: `document` e i nodi (alberi, selettori,
`innerHTML`, `classList`, `style`, `dataset`), eventi con cattura e
propagazione, timer, `location`/`history`, `localStorage`, `fetch` e
`XMLHttpRequest`. Gli script di una pagina girano su un thread loro (stack
da 2 MiB), che poi ogni 15 ms passa agli script i clic, i tasti e i timer
scaduti; un ciclo infinito viene interrotto dopo un tetto di passi e la
pagina resta usabile. Ciò che gli script scrivono in `console` va nel
giornale (`log`). Limiti: niente JIT (le pagine pesanti sono lente), `await`
è sincrono, niente canvas, WebSocket, worker e moduli ES.

Ogni pagina si carica su un thread suo (pagina, fogli di stile, poi le
immagini quattro alla volta) e viene impaginata di nuovo quando arrivano le
immagini o la finestra cambia misura; i caratteri delle pagine (Google Sans
Flex in varie misure, normale e grassetto, e DejaVu Sans Mono) sono in
`sys:/system/webfonts.bin`. Per provarlo sull'host:

```sh
cd system && cargo run --release --target x86_64-unknown-linux-gnu \
    -p arcweb --example render -- pagina.html 1024 pagina.ppm
```

## Il Negozio e le app installabili

Il Negozio installa programmi veri, separati dal sistema: ognuno gira in
modalità utente nel suo spazio di indirizzi e apre una finestra sulla
scrivania con le chiamate di sistema `WIN_*` (vedi
[programmi](programmi.md#programmi-con-una-finestra)). Ce ne sono sei:

| App | Categoria | |
|---|---|---|
| 2048 | Giochi | le tessere da unire, con animazioni e record |
| Campo minato | Giochi | tre livelli, bandierine con il tasto destro |
| Serpente | Giochi | sempre più veloce, record salvato |
| Sudoku | Giochi | partite sempre nuove con una sola soluzione, matita, errori in rosso |
| Tris | Giochi | contro il computer (in *Difficile* non perde mai) |
| Disegno | Creatività | matita, pennello, gomma, forme, secchiello, colori; salva in `Immagini` |

e due programmi per il Terminale, BusyBox e Lua (Linux). Ogni pacchetto è una
cartella `sys:/store/<id>` con il programma (`app`), l'icona (`icon.qoi`) e
una scheda (`info`: nome, categoria, descrizione, versione, tipo). Installare
copia questi file in `sys:/apps`; da lì le app compaiono in Applicazioni. Dal
Terminale: `install`, `install <nome>`, `uninstall <nome>`.

## Schermo e suono

Con un driver video (`bochsvga`, `vmsvga`, `virtiogpu`: le schede delle
macchine virtuali) *Impostazioni → Schermo* elenca le risoluzioni comuni
che lo schermo offre; un clic la cambia subito, e arcOS la riprende a ogni
avvio. Senza driver resta la risoluzione scelta dal firmware. Dal
Terminale: `display` elenca, `display 1920x1080` cambia.

Con una scheda audio (`hda`, `ac97`) la scrivania suona una breve melodia
quando compare (si può spegnere), il volume si regola dal centro di
controllo e dalle Impostazioni, e Musica e Orologio suonano. Dal Terminale:
`play file.wav`, `tone 440 500`, `volume 60`.

## Com'è fatta

L'interfaccia è nel kernel, in `system/kernel/src/gui`; il disegno è una
libreria condivisa con i programmi, `system/libs/arcgfx`. Niente librerie
esterne.

| File | Contenuto |
|---|---|
| `arcgfx` (`gfx.rs`, `font.rs`, `qoi.rs`) | superfici e disegno: rettangoli arrotondati antialiasati, trasparenze (anche su livelli trasparenti), sfumature, ombre, sfocatura, linee, archi, ridimensionamento; il testo dall'atlante dei glifi; le immagini QOI (lettura e scrittura). Solo aritmetica intera |
| `text.rs`, `res.rs` | i caratteri e le icone di sistema, gli sfondi |
| `splash.rs`, `setup.rs`, `desktop.rs` | avvio, configurazione guidata, scrivania con dock, Applicazioni, notifiche, centro di controllo e animazioni |
| `wm.rs` | le finestre e l'interfaccia `App` delle app |
| `window.rs` | le finestre dei programmi in modalità utente |
| `ui.rs` | componenti comuni (pulsanti, cursori, interruttori, schede), animazioni (`Anim`, curve di accelerazione), appunti, il ciclo degli eventi |
| `textarea.rs` | il testo su più righe di Editor, Note |
| `apps/` | le app di sistema |

Il disegno avviene in un buffer in memoria; sullo schermo si copia solo la
parte che cambia. Il puntatore è disegnato direttamente sullo schermo, così
muoverlo non ridisegna nulla. I pannelli "vetro smerigliato" mostrano una
copia sfocata dello sfondo calcolata una volta sola. I clic del mouse vengono
messi in coda dal driver, così un clic rapido non si perde anche se la
scrivania sta disegnando un'animazione.

La console di testo continua a esistere: con la grafica attiva è una griglia di
80x25 caratteri che la finestra Terminale disegna, e la tastiera passa alla
shell solo quando il Terminale è in primo piano.

## Le risorse

Caratteri, icone, sfondi e suoni sono generati in fase di sviluppo da script
Python (`tools/assets/`) e salvati nel repository; per ricostruire arcOS non
servono.

- `gen_fonts.py` → `system/kernel/assets/fonts.bin`: Inter e DejaVu Sans
  Mono (licenze in `system/kernel/assets/LICENSES.md`); una copia va in
  `sys:/system/fonts.bin` per i programmi.
- `gen_icons.py` → `rootfs/system/gui/icons.qoi`: oltre 70 icone, disegnate
  da zero nello stile arc (squircle con sfumatura e simbolo); con `--store`
  le icone delle app del Negozio.
- `gen_wallpapers.py` → `rootfs/system/gui/wallpapers/`: tre sfondi
  (Dolomiti, Alba, Notte) dipinti con rumore e sfumature.
- `gen_sounds.py` → `rootfs/system/sounds/avvio.wav`, il suono d'avvio, e i
  tre brani di `rootfs/home/Musica`, composti e sintetizzati dallo script.

`make assets` li rigenera (servono Python 3, Pillow e numpy).

## Provarla

`make run` apre arcOS in una finestra di QEMU. `make shots` avvia arcOS senza
schermo, lo guida con mouse e tastiera (`scripts/gui-shots.py`, attraverso
QMP) lungo `scripts/gui/tour.txt` (configurazione, ogni app, il Negozio che
installa e avvia un gioco) e salva le schermate in `build/shots/`; con
`SCREEN=1920x1080` le coordinate del giro si riferiscono a uno schermo di
quella misura.
