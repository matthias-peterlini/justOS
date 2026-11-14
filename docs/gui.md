# L'interfaccia grafica

Quando il loader trova uno schermo grafico di almeno 800x600, justOS si avvia
in modalità grafica. Sullo schermo non compare mai testo tecnico: il kernel
scrive i suoi messaggi solo sulla porta seriale e nel giornale di sistema, e
le righe di comando si vedono soltanto aprendo il Terminale. Anche un errore
fatale mostra una schermata semplice («justOS si è dovuto fermare»), con i
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

Le scelte sono in `sys:/system/settings`; *Impostazioni → Generale →
Configurazione iniziale* la ripropone al prossimo avvio. Il CD live non
conserva nulla, quindi mostra la configurazione ogni volta.

## La scrivania

Presa a modello da macOS:

- **Niente barra in alto**: l'ora e la data fluttuano al centro, con le
  cifre in pixel morbidi (bianche o nere secondo lo sfondo, con una
  piccola ombra). Un clic apre il **centro notifiche** con il calendario
  del mese e le notifiche ricevute.
- **Dock**: un unico pannello opaco del tema. A sinistra il marchio **J▮**
  su un riquadro scuro, che apre il menu di sistema
  (*Info su justOS*, *Impostazioni di sistema*, *Negozio*, *Monitor
  attività*, *Desktop e finestre*, *Schermata*, *Blocca schermo*,
  *Sospendi* quando il computer può, *Riavvia*, *Spegni*). Poi File,
  Applicazioni, Editor di testo, Note, Calendario, Foto, Musica,
  Calcolatrice, Negozio, Impostazioni e, dopo un separatore, le app aperte
  che non ci sono già e il Cestino. Le icone **si ingrandiscono** sotto il
  puntatore, quella di un'app che parte **rimbalza**, un quadratino arancione
  segna le app aperte; un clic su un'app aperta la porta davanti (o la riapre se è
  ridotta). Con molte app aperte le icone si rimpiccioliscono perché il
  dock stia nello schermo. Il dock cambia forma per mostrare quello che
  succede (vedi [La barra dinamica](#la-barra-dinamica)).
- **Stato e centro di controllo**: a destra nel dock, in un incavo, i
  simboli in pixel morbidi della rete (spento se non è collegata), del
  suono e, sui portatili, della batteria (colorata secondo la carica). Un
  clic apre il **centro di controllo**, un pannello sopra il dock fatto di
  schede: il nome di chi usa il computer su un disco arancione, con la
  carica (o *Alimentatore*) e un riquadro con l'ingranaggio delle
  impostazioni; i riquadri *Rete* (blu quando è collegata), *Bluetooth*
  e *Non disturbare* (viola, con un interruttore); due barre a celle
  orizzontali per luminosità e volume (l'altoparlante a sinistra spegne e
  riaccende il suono); i pulsanti *Blocca*, *Riavvia* e *Spegni*
  (arancione). Le misure sono quelle di un disegno per 1024×768,
  rimpicciolite fino al 60% sugli schermi più piccoli.
- **Widget** (tasto destro → *Mostra i widget*): saluto, calendario del
  mese, stato del sistema.
- **Applicazioni** (come Launchpad, dal dock o con Ctrl+Spazio): tutte le app,
  comprese quelle installate dal Negozio, con una ricerca; si scrive e Invio
  apre la prima. La ricerca è anche quella di **Spotlight**: sopra le app
  compaiono i **calcoli** (`12*3+4` → `= 40`, Invio lo copia), le
  **impostazioni** per parola («sfondo», «volume», «password», «processore»...)
  e i **file e le cartelle** della home per nome, che si aprono con l'app
  giusta. La home viene letta una volta sola all'apertura della ricerca.
- **Notifiche**: un'app installata, un timer scaduto, un file salvato, un
  programma che si è chiuso per un errore. Quelle brevi escono dal dock
  (vedi [La barra dinamica](#la-barra-dinamica)), le altre in alto a
  destra. Spariscono da sole o con un clic e restano nel centro notifiche.
- **Tasto destro** sulla scrivania: cambiare sfondo, mostrare o
  nascondere i widget, impostazioni dello schermo, informazioni.
- **Finestre**: un pannello opaco del tema (scuro o chiaro) con il nome
  dell'app in alto a sinistra e, in alto a destra, tre riquadri con un
  simbolo in pixel morbidi: riduci e schermo intero grigi, chiudi
  arancione; il contenuto sta su un incavo. Si spostano trascinando
  la riga del titolo (doppio clic per lo schermo intero), molte si
  ridimensionano dall'angolo in basso a destra. A schermo intero (anche
  **F11**) la finestra prende tutto lo schermo, l'ora passa nella sua riga
  del titolo e il dock compare portando il puntatore in basso.
  **Alt+Tab** passa alla finestra successiva, **Ctrl+Q** chiude quella in
  primo piano.
- **Snap**, come in Windows: una finestra trascinata sul bordo sinistro o
  destro occupa quella metà dello schermo, in un angolo un quarto, sul bordo
  in alto tutto lo schermo; mentre si trascina un riquadro chiaro mostra
  dove andrà.
  Da tastiera (finestre ridimensionabili) **Ctrl+Alt+←/→** per le metà,
  **Ctrl+Alt+U/I/J/K** per i quarti, **Ctrl+Alt+↑** ingrandisce e
  **Ctrl+Alt+↓** ripristina. Trascinata via, la finestra torna alla sua
  misura di prima.
- **Desktop virtuali e vista d'insieme**: **F3** (o **logo+Tab**, o
  *Desktop e finestre* nel menu J▮) mostra in alto i desktop e sotto
  le finestre di quello attuale affiancate; un clic su una finestra la porta
  davanti, trascinata su un desktop ci si sposta, trascinata sul **+** ne
  crea uno nuovo (fino a sei); la × su un desktop lo toglie e le sue
  finestre passano a quello prima. **Ctrl+Alt+PagSu/PagGiù** (o
  **logo+Ctrl+←/→**) passano al desktop accanto, **Ctrl+Alt+Maiusc+←/→**
  ci portano la finestra in primo piano (oltre l'ultimo ne crea uno). Con
  più desktop, dei pallini sotto l'ora dicono dove si è; il dock e Alt+Tab
  portano al desktop della finestra scelta.
- **Trascinare i file**: da File si trascinano uno o più file (Ctrl+clic e
  Maiusc+clic selezionano, Ctrl+A tutto) su una cartella, su una risorsa a
  sinistra, su una scheda o su un'altra finestra di File. Sullo stesso
  volume si spostano, tra volumi diversi si copiano; tenendo **Ctrl** (o
  Alt) si copia sempre e compare un **+** verde, con **Maiusc** si sposta
  sempre. Rilasciati sul Cestino del dock ci finiscono dentro; sull'icona di
  un'app (Editor, Foto, Musica, File) si aprono lì; sul
  Terminale ne scrivono il percorso; nell'Editor si aprono. Le app del
  Negozio che lo chiedono (Disegno) ricevono i file, solo in lettura.
- **Schermata**: il tasto **Stamp** (o *Schermata* nel menu J▮) salva
  lo schermo in `sys:/home/Immagini` come `Schermata <data> alle <ora>.qoi`,
  che Foto apre subito.
- **Blocco**: **logo+L** o *Blocca schermo* nel menu J▮ coprono tutto
  con l'ora sullo sfondo sfocato. Con una password (*Impostazioni →
  Blocco e password*) justOS si avvia bloccato, si blocca da solo dopo 2, 5 o 15
  minuti senza usarlo e prima di ogni sospensione, così al risveglio chiede
  la password; senza password basta Invio. La password non è salvata
  in chiaro: nelle impostazioni c'è un sale casuale e 20 000 passaggi di
  SHA-256.
- **Aggiornamenti**: con la rete, all'avvio e una volta al giorno justOS
  legge l'indice firmato degli aggiornamenti. Una versione nuova si
  scarica e si scrive nell'altro slot di sistema; con gli aggiornamenti
  automatici succede da sola, poi una notifica chiede di riavviare. In
  *Info sistema* il pulsante passa da *Cerca aggiornamenti* a *Installa*,
  *Scarico…*, *Installo…* e *Riavvia*. Accanto ci sono *Annulla* e *Torna a
  <versione precedente>*. Se la versione nuova non parte, justOS torna da
  solo alla precedente e lo dice. Una release su GitHub che l'indice non ha
  ancora è solo annunciata.

### Lo stile a pixel morbidi

Finestre, dock, menu, notifiche e centro di controllo non sono più di vetro:
sono superfici **opache**, senza sfocatura dello sfondo né contorni scuri.

- **Superfici** (`ui::panel`): una sfumatura dall'alto al basso
  (`panel_top` → `panel_bottom`), una linea sottile tutt'intorno e un
  riflesso chiaro lungo il bordo superiore (`Canvas::sheen_round`); sotto,
  un'ombra morbida (`ui::drop_shadow`). Il raggio è 26 per finestre,
  centro di controllo e centro notifiche, 28 per il dock, 18 per gli incavi e le
  schede (`ui::well_pane`: il colore `well` con una linea sottile).
- **Riquadri** (`ui::tile`): i quadratini che reggono un simbolo (i tre
  pulsanti della finestra, l'ingranaggio, le icone delle sezioni, i
  riquadri del centro di controllo). Hanno il raggio pari a un quarto del
  lato, una sfumatura verticale, il riflesso in alto e una piccola ombra.
  Quello arancione è `ui::ORANGE_GRAD`; il grigio, `ui::knob_grad()`.
- **Simboli a pixel morbidi**: ogni simbolo è poche righe di testo (`#` è
  una cella piena, `.` vuota, qualsiasi altro carattere una cella con un
  colore a parte), disegnato da `Canvas::soft_grid` tramite
  `ui::soft_pixels_at`, `soft_pixels_in` (centrato in un rettangolo) o
  `soft_pixels_keyed`. La regola degli angoli: un angolo **in fuori** si
  arrotonda di mezza cella; dove due celle piene si incontrano intorno a
  una vuota, l'angolo **in dentro** si raccorda con una curva di 0,38
  celle. La cella si dà in millesimi di unità, così lo stesso simbolo vale
  a ogni scala; l'angolo in alto a sinistra si allinea a un pixel intero,
  quindi celle di un numero intero di pixel escono nitide. Sono fatti così
  i simboli del dock e del centro di controllo, quelli dei pulsanti della
  finestra e delle sezioni delle Impostazioni.
- **Cursori a celle** (`ui::cell_bar`, `ui::slider`): una fila di 16 celle
  arrotondate che si riempiono da sinistra; piene in arancione (volume,
  accento) o nel colore del testo (luminosità).
- **Orologio**: le cifre sono pixel morbidi di 3×5 celle (`DIGITS` in
  `desktop.rs`, cella da 10 unità), con la data sotto nel carattere
  normale. Il colore è bianco o nero secondo la luminosità dello sfondo
  sotto l'orologio, con un'ombra leggera fatta ridisegnando le cifre un
  po' più in basso.

### Il tema scuro e chiaro

Ci sono due tavolozze, scura e chiara (`ui::DARK` e `ui::LIGHT`), e si
sceglie in *Impostazioni → Aspetto → Tema*. La scelta è nelle impostazioni
come `theme=dark` o `theme=light` (`settings.rs` la legge all'avvio e la
scrive a ogni salvataggio) e `ui::set_theme` la rende attiva; senza
scelta il tema è scuro.

I colori **non sono costanti**: si leggono con le funzioni di `ui`, che
guardano la tavolozza del tema in uso: `ui::text()`, `text_dim()`,
`text_faint()`, `well()`, `pane()`, `line()`, `sheen()`, `sel()`,
`hover()`, `cell()`, `panel_top()`, `panel_bottom()`, `knob_grad()`,
`knob_glyph()` e `shadow_strength()` per le ombre (più deboli nel chiaro).
Il tema attivo si legge con `ui::theme()`. Cambiando tema, Impostazioni
restituisce `Action::RedrawAll` e la scrivania ridisegna tutto lo schermo.

Come sceglie i colori un'app:

- testo, linee, selezioni, passaggio del puntatore e campi vanno sempre
  presi da queste funzioni, mai scritti come bianco o nero con
  trasparenza, perché nell'altro tema non si leggerebbero;
- vanno bene colori fissi solo per ciò che è uguale in entrambi i temi:
  l'arancione (`ui::ORANGE`, `ORANGE_GRAD`), il rosso e il verde di stato,
  i riquadri colorati, le immagini dell'app. Se un colore deve essere più
  scuro nel tema chiaro (il rosso dei festivi in Calendario) l'app guarda
  `ui::theme()` e ne ha due;
- i componenti comuni (`ui::button`, `toggle`, `slider`, `segmented`,
  `search_field`, `text_field`) e le superfici (`panel`, `well_pane`,
  `tile`) seguono già il tema.

### Le Impostazioni

A sinistra una barra con la ricerca e sette sezioni, ognuna con il suo
riquadro colorato e un simbolo: *Generale*, *Aspetto*, *Schermo*, *Suono*,
*Tastiera*, *Blocco e password*, *Privacy*. A destra le **schede**
(`Card`) della sezione, ognuna con più **righe** (`Row`): un'etichetta, una
riga di spiegazione facoltativa e un controllo (`Ctl`) a destra, oppure
sotto per quelli larghi (sfondi, risoluzioni). Il tema si sceglie da due
anteprime disegnate, scuro e chiaro. Le frecce su e giù cambiano sezione,
Pag su e Pag giù scorrono.

La **ricerca** si apre scrivendo (Esc la chiude): ignora maiuscole e
accenti (`fold`), cerca nelle etichette delle righe e nei nomi delle
sezioni e mostra i risultati raggruppati per sezione. Le frecce scelgono un
risultato; Invio o un clic aprono la sezione e accendono per un momento la
riga trovata.

Per **aggiungere una riga** si modifica `Prefs::cards` in
`system/kernel/src/gui/apps/prefs.rs`: nella sezione giusta si aggiunge
`row("Etichetta", "Spiegazione", Ctl::…)` a una scheda (o una nuova
`card(vec![...])`). Va bene un controllo che già c'è (`Ctl::Info` per un
valore, `Ctl::Text` per una riga di testo, `Ctl::Themes`, `Ctl::Layout`…).
La ricerca la trova da sola, perché cerca nelle righe di `cards`. Un
controllo nuovo vuole una variante di `Ctl` e il suo disegno in
`draw_ctl`, la misura in `ctl_size`, le zone cliccabili in `parts` e
l'azione in `act`. Se cambia qualcosa dei colori, `act` restituisce
`Action::RedrawAll`. Una sezione nuova si aggiunge a `Section` e a
`SECTIONS` con nome, sfumatura e simbolo.

### La barra dinamica

Il dock in basso si adatta a quello che succede: invece di aprire
finestrelle altrove, cambia forma per un momento e poi torna com'era. È
fatto nello stesso stile a pixel morbidi del resto (pannello opaco, incavi,
celle arancioni), non copia quello di altri sistemi. Il codice è in
`system/kernel/src/gui/desktop/bar.rs`.

| Cosa succede | Come cambia il dock |
|---|---|
| a riposo | come sempre: nessun pixel diverso |
| un programma si avvia | la sua icona diventa una **capsula** con il nome, *Si sta avviando…* (o una nota: *Si avvia con Wine…*, *Prima apertura: preparo Windows…*, *Si estrae, ci vuole un po'…*) e una fila di celle che scorre; sparisce quando arriva la finestra, quando il programma finisce senza finestra o dopo 120 secondi. Aprirlo di nuovo mentre parte non ne avvia un secondo: la capsula torna davanti e l'icona rimbalza |
| volume o luminosità | il dock diventa una **barra di livello** in basso al centro: il simbolo, 16 celle e il valore; resta 1,5 secondi dopo l'ultimo cambio. Un clic sulle celle sceglie il livello. Viene dai tasti del volume (su, giù, muto), dalle Impostazioni o da un'app; non compare con il centro di controllo aperto, che ha già i suoi cursori |
| una notifica breve (titolo fino a 40 caratteri, testo fino a 60) | il dock **cresce verso l'alto** con una riga: icona, titolo, testo, *Chiudi* e, se viene da un'app, *Apri* arancione; resta 4,5 secondi, di più finché il puntatore ci sta sopra. Quelle lunghe, o con il dock nascosto, vanno in alto a destra come prima |
| musica | l'icona di Musica diventa una capsula con il titolo, *Musica · 0:42* e un equalizzatore di 5 barre a 8 passi al secondo; un clic apre Musica |
| timer dell'Orologio | capsula con il tempo che manca; quando suona pulsa in arancione per 30 secondi ed esce davanti alle altre |
| a schermo intero | il dock è nascosto; se c'è un'attività resta una **pillola** sottile in basso al centro con l'ora e l'equalizzatore, le celle o il tempo del timer. Un clic apre l'app; portando il puntatore sul bordo in basso torna il dock |
| due attività o più | nel dock ce n'è una (la più recente), le altre aspettano in una **bolla** a destra del dock, con il numero quando sono due o più; un clic sulla bolla le scambia |

Come funziona:

- Ogni attività (`Activity`) ha il suo tipo (`ActKind`: `Starting`, `Music`,
  `Timer`, `Volume`, `Brightness`, `Note`), una scadenza facoltativa e un
  `Morph`, un valore da 0 a 1000 che va verso l'obiettivo in 250 ms. La
  forma del dock passa da una all'altra con questo valore: le capsule si
  allargano dall'icona, la barra di livello e la riga della notifica
  sfumano dal dock.
- Le app dicono cosa fanno con `App::activity()` (`wm::Live::Music` o
  `Live::Timer`); `Desktop::bar_tick`, dieci volte al secondo, le
  confronta con quelle che ci sono, legge volume e luminosità e toglie le
  scadute.
- Si ridisegna il meno possibile: quando cambia la forma, il rettangolo
  vecchio unito al nuovo (`damage_bar`, che ricorda l'ultimo disegnato);
  altrimenti solo le barre dell'equalizzatore, le celle o le cifre del
  timer.
- Un'app nuova che vuole la sua capsula implementa `activity()` restituendo
  una variante di `Live`; se serve un tipo nuovo, si aggiunge a `Live`, ad
  `ActKind` e a `draw_capsule`.
- Dalla shell `notify <titolo> <testo> [icona]` manda una notifica per
  provarla.

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
| barra dinamica | il dock cambia forma in 250 ms: capsule, barra di livello, riga della notifica, pillola, bolla |
| menu e centro di controllo | si aprono in un attimo, dal dock verso l'alto |
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
| File | sfoglia i volumi: risorse a sinistra, cartelle e file a destra; doppio clic apre una cartella o il file con l'app giusta; **schede** (Ctrl+T o il **+**, Ctrl+W chiude, Ctrl+Tab passa alla successiva, *Apri in una nuova scheda* sulle cartelle); tasto destro per aprire, rinominare (anche F2), spostare nel Cestino (Canc), creare una cartella; nel Cestino eliminare o svuotare; **selezione multipla** (Ctrl+clic, Maiusc+clic, Ctrl+A) e **trascinamento** tra cartelle, schede e finestre |
| Editor di testo | file di testo con selezione, copia e incolla, scorrimento; Nuovo, Apri, Salva (Ctrl+N, Ctrl+O, Ctrl+S) con la scelta del file; alla chiusura salva da solo |
| Note | note in `sys:/home/Note`, la prima riga è il titolo, ricerca, si salvano mentre si scrive |
| Promemoria | cose da fare, spuntate con un clic (`sys:/home/Promemoria.txt`) |
| Calendario | un mese alla volta con le feste italiane (anche Pasqua e Pasquetta) e i propri eventi («9:30 Riunione»), salvati in `sys:/home/Calendario.txt` |
| Orologio | l'ora qui e in altre città, cronometro con i giri, timer che suona e avvisa |
| Calcolatrice | come quella di macOS, anche da tastiera; calcoli in virgola fissa con dodici decimali |
| Foto | le immagini di `Immagini` e gli sfondi, in miniatura e a tutto schermo |
| Musica | i file WAV di `Musica` e i suoni di sistema: riproduzione, pausa, brano precedente e successivo, barra di avanzamento, volume; tre brani di esempio |
| Anteprima | un'immagine sola, aperta da File |
| Negozio | le app di `sys:/store` per categoria, con la loro pagina; installa, apre, rimuove |
| Impostazioni | sette sezioni con ricerca: nome e configurazione iniziale, tema e sfondo, risoluzione e dimensione dello schermo, volume e suono all'avvio, tastiera, password e blocco, permessi delle app (vedi [Le Impostazioni](#le-impostazioni)) |
| Monitor attività | i programmi in esecuzione, il processore e la memoria nell'ultimo minuto; un programma si può terminare |
| Info sistema | panoramica, dispositivi con i loro driver, dischi con lo spazio libero |
| Terminale | la shell di justOS: l'unico posto dove si vedono le righe di comando |

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
che lo schermo offre; un clic la cambia subito, e justOS la riprende a ogni
avvio. Senza driver resta la risoluzione scelta dal firmware. Dal
Terminale: `display` elenca, `display 1920x1080` cambia.

Sugli schermi ad alta densità l'interfaccia raddoppia: tutto è misurato in
*unità*, e da 2560×1440 in su (se restano almeno 800×600 unità) ogni unità
è di 2×2 pixel. Il testo non viene ingrandito ma disegnato da un secondo
atlante di glifi fatto al doppio della misura (`fonts@2x.bin`), le icone e
gli sfondi sono ridotti dagli originali grandi, il puntatore è ridisegnato
alla nuova misura: stessa disposizione, il doppio dei dettagli. *Impostazioni
→ Schermo* (quando lo schermo è abbastanza grande) sceglie tra *Auto*,
*100%* e *200%*; dal Terminale `display scale auto|1|2`. La scelta resta
nelle impostazioni.

Con una scheda audio (`hda`, `ac97`) la scrivania suona una breve melodia
quando compare (si può spegnere), il volume si regola dal centro di
controllo e dalle Impostazioni, e Musica e Orologio suonano. Dal Terminale:
`play file.wav`, `tone 440 500`, `volume 60`.

## Com'è fatta

L'interfaccia è nel kernel, in `system/kernel/src/gui`; il disegno è una
libreria condivisa con i programmi, `system/libs/justgfx`. Niente librerie
esterne.

| File | Contenuto |
|---|---|
| `justgfx` (`gfx.rs`, `font.rs`, `qoi.rs`) | superfici e disegno: rettangoli arrotondati antialiasati, il riflesso sul bordo (`sheen_round`), i pixel morbidi (`soft_grid`), trasparenze (anche su livelli trasparenti), sfumature, ombre, sfocatura, linee, archi, ridimensionamento; il testo dall'atlante dei glifi; le immagini QOI (lettura e scrittura). Solo aritmetica intera |
| `text.rs`, `res.rs` | i caratteri e le icone di sistema, gli sfondi |
| `splash.rs`, `setup.rs`, `desktop.rs` | avvio, configurazione guidata, scrivania con dock, Applicazioni, notifiche, centro di controllo e animazioni |
| `wm.rs` | le finestre e l'interfaccia `App` delle app |
| `window.rs` | le finestre dei programmi in modalità utente |
| `ui.rs` | il tema e le sue tavolozze, superfici (pannelli, incavi, riquadri), pixel morbidi, componenti comuni (pulsanti, cursori a celle, interruttori, schede), animazioni (`Anim`, curve di accelerazione), appunti, il ciclo degli eventi |
| `textarea.rs` | il testo su più righe di Editor, Note |
| `apps/` | le app di sistema |

Il disegno avviene in un buffer in memoria; sullo schermo si copia solo la
parte che cambia. Un'app può dire quale parte del suo contenuto è cambiata
(`App::changed`): il cursore che lampeggia nel Terminale o nell'Editor
ridisegna una cella, non la finestra, e l'output del Terminale solo le
righe nuove. Il puntatore è disegnato direttamente sullo schermo, così
muoverlo non ridisegna nulla. Una copia sfocata dello sfondo, calcolata una
volta sola, serve dietro Applicazioni, la vista d'insieme e il blocco. I clic del mouse vengono
messi in coda dal driver, così un clic rapido non si perde anche se la
scrivania sta disegnando un'animazione.

La console di testo continua a esistere: con la grafica attiva è una griglia di
80x25 caratteri che la finestra Terminale disegna, e la tastiera passa alla
shell solo quando il Terminale è in primo piano.

## Le risorse

Caratteri, icone, sfondi e suoni sono generati in fase di sviluppo da script
Python (`tools/assets/`) e salvati nel repository; per ricostruire justOS non
servono.

- `gen_fonts.py` → `system/kernel/assets/fonts.bin`: Inter e DejaVu Sans
  Mono (licenze in `system/kernel/assets/LICENSES.md`); una copia va in
  `sys:/system/fonts.bin` per i programmi. Con `--sharp` gli stessi
  caratteri al doppio della misura (`fonts@2x.bin.gz`, per gli schermi ad
  alta densità; i programmi lo trovano in `sys:/system/fonts@2x.bin`).
- `gen_pixicons.py` → `rootfs/system/gui/icons.qoi`: le icone a pixel di
  justOS, quasi novanta, su una griglia: app in un riquadro di 20×20 celle
  con la sfumatura, glifi di sistema in 12×12, angoli esterni arrotondati
  di mezza cella e raccordi di 0,38 negli angoli interni (`pixgrid.py`).
  Le icone di partenza sono gli SVG di `tools/assets/src/icone-justos`
  (letti cella per cella); le altre sono disegnate nello script come
  testo, un carattere per cella. Con `--store` le icone dei giochi del
  Negozio. Basta Pillow.
- `gen_pixel.py` → `rootfs/system/gui/wallpapers/pixel.qoi`: lo sfondo
  predefinito, le Dolomiti al tramonto in grandi pixel con il retino
  ordinato (Bayer) nei colori caldi di justOS; basta Pillow.
- `gen_wallpapers.py` → `rootfs/system/gui/wallpapers/`: due sfondi
  (Tramonto, Notte) dipinti con rumore e sfumature.
- `gen_sounds.py` → `rootfs/system/sounds/avvio.wav`, il suono d'avvio, e i
  tre brani di `rootfs/home/Musica`, composti e sintetizzati dallo script.

`make assets` li rigenera (servono Python 3, Pillow e numpy).

## Provarla

`make run` apre justOS in una finestra di QEMU. `make shots` avvia justOS senza
schermo, lo guida con mouse e tastiera (`scripts/gui-shots.py`, attraverso
QMP) lungo `scripts/gui/tour.txt` (configurazione, ogni app, il Negozio che
installa e avvia un gioco) e salva le schermate in `build/shots/`; con
`SCREEN=1920x1080` le coordinate del giro si riferiscono a uno schermo di
quella misura.
