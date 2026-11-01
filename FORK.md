# FORK.md: zuerst lesen

Diese Datei gilt für alle Agents, die in diesem Fork arbeiten. Lies sie **vor** `AGENTS.md`, `CONTRIBUTING.md` und allen anderen `.md`-Dateien. Bei Widersprüchen gilt die Reihenfolge:

1. Was der Besitzer des Forks (Trialalloy99) dir direkt sagt
2. Diese Datei
3. `AGENTS.md` und die übrigen Projektdateien

Eine Regel aus `AGENTS.md` bleibt immer bestehen: Agents öffnen nie selbst Pull Requests und posten nichts auf GitHub.

## Ziel

Der Besitzer will so schnell wie möglich eine **spielbare Version von Pumpkin** für sich und seine Freunde. Der Fork ist `Trialalloy99/Pumpkin_Fork_Trialalloy99`, das Original ist `Pumpkin-MC/Pumpkin`. Wir bauen fehlende Entities und Items selbst ein, Batch für Batch. PRs ans Original kommen vielleicht später, aber nicht als nächster Schritt. Arbeite trotzdem so, dass jeder Commit später als PR taugen würde.

Tempo ist wichtig. Ein einfacher Mob wie der Vex hat etwa 10 Minuten Coding gebraucht, mit allen Checks knapp eine Stunde. Plane danach und verliere dich nicht in Nebenbaustellen.

## Arbeitsablauf

- **Batches:** Immer 5 bis 6 Entities (oder Items) pro Batch. Erst alle umsetzen, dann am Ende der Batch gemeinsam testen.
- **Pro Mob:** Vanilla-Code holen (siehe unten), portieren, dann `cargo check -p pumpkin-core`. Mehr nicht.
- **Am Ende der Batch:**
  - `cargo fmt`
  - `cargo clippy -p pumpkin-core --all-targets --all-features`
  - `cargo test -p pumpkin-core`
  - Ein Boottest mit `cargo build -p pumpkin`, wobei der Server in einem Scratch-Ordner läuft
  - Eine Checkliste für den Test im Spiel, im Stil „mach X, erwarte Y“
- **Branches:**
  - `playable` ist der Integrations-Branch, auf dem die Freunde spielen.
  - Jede Batch bekommt einen eigenen Branch von `playable` (z. B. `batch-2`). Er wird nach dem Test in `playable` gemergt.
  - Einzelne Mob-Branches wie `feat/vex-ai` bleiben sauber auf `master`, damit sie später als PR taugen.
- **Commits:**
  - Ein Commit pro Mob, Conventional Commits mit Scope, z. B. `feat(entity): port guardian attack goal`.
  - Autor ist `Trialalloy99 <maxi.ratnik@gmail.com>`. Keine `Co-Authored-By`- oder Tool-Zeilen.
- **Code:** Keine Kommentare im Code, außer wenn sonst niemand eine Vanilla-Eigenheit verstehen würde. Der Code soll aussehen wie der Rest des Projekts.
- **Geschwindigkeitstests:** Nur nach großen Features, im Vergleich mit Vanilla und Paper auf demselben Seed. Wenn der optimierte Build länger als 10 Minuten braucht, fällt der Geschwindigkeitstest weg. Das Skript dafür steht unten.
- **Push:** Nach jeder Batch auf den Fork pushen.

## Eigener Tracker

Unser Fortschritt steht auf **https://claude.ai/artifact/KAHK4pXmbbEhd9nXiBuobN** („Pumpkin Fork Tracker“). Die Seite enthält alle Einträge der offiziellen Seite https://pumpkinmc.org/tracker/ (Stand der eingebetteten Daten: 2026-10-31) und dazu unseren eigenen Stand.

- **Wie aktualisieren:** Agents schreiben über das `ArtifactData`-Tool in die Collection `progress`.
- **Dokument-ID:** `<kategorie>:<id>`, z. B. `entities:vex`. Kategorien: `entities`, `blocks`, `items`, `commands`, `redstone`, `combat`, `world`.
- **Felder:** `{cat, id, status, batch, note, updated}`
  - `status`: `next` (nächste Batch), `wip` (in Arbeit), `done` (bei uns fertig)
  - `batch`: Zahl
  - `note`: kurzer Text
  - `updated`: ISO-Zeit
- **Wann:** Beim Start einer Batch die Einträge auf `wip` setzen, nach dem Commit auf `done`. Vorher lesen und `if_version` mitgeben.
- **Offizielle Daten erneuern:** Die Einträge kommen aus https://github.com/Pumpkin-MC/Pumpkin-Website/blob/master/src/pages/tracker/data.json. Bei Bedarf neu einbetten und die Seite unter derselben URL neu veröffentlichen.

## Stand

| Batch | Inhalt | Stand |
|---|---|---|
| 1 | Vex (plus die Vex-Beschwörung beim Evoker) | fertig auf `feat/vex-ai` und `playable`; Test im Spiel steht aus |
| Commands | `/locate` (alle POI-Typen, POIs aus geladenen Chunks), `/perf` | fertig auf `batch-commands` |
| 2 | Guardian, Elder Guardian, Illusioner, Vindicator, Ravager, Endermite | als Nächstes |

Aus dem Tracker entfernt, weil Vanilla sie auf einem dedizierten Server nicht hat: `/publish` und `/unpublish` (nur Einzelspieler) sowie `/jfr` (Java Flight Recorder, braucht eine JVM).

Beim Vex fehlt noch: Vanilla hat ein zweites Umschau-Ziel (`LookAtPlayerGoal` mit `Mob`, Reichweite 8), das bei uns fehlt. `LookAtEntityGoal` kann nur einen einzigen Entity-Typ als Ziel.

Upstream arbeiten andere bereits an: Silverfish (#3886), Phantom (#3715), Spider und Cave Spider (#4368), Witch (#4393), Pufferfish (#4050), Zoglin und Hoglin (#4095), Ausrüstung beim Spawnen (#4391). Den Endermite hat AdmerPRO beansprucht (#4255). Wenn wir diese Mobs anfassen, gibt es beim nächsten Merge von upstream Konflikte. Prüfe deshalb vor jeder Batch die offenen PRs.

## Wichtig zu wissen

- **Vanilla-Code:** Version 26.3. Lade das Server-JAR über das Mojang-Manifest herunter und dekompiliere mit Vineflower (`-only=<paket>`), alles im Scratchpad und nicht im Repo. Die Klassen sind nicht obfuskiert. Für Mobs liegt der Code in `net/minecraft/world/entity/monster/...`.
- **Java:** Für Vineflower reicht Java 21. Ein Vanilla-Server 26.3 braucht Java 25, das hier nicht installiert ist.
- **Bauzeiten im Container:**
  - `cargo check -p pumpkin-core`: etwa 3 Minuten beim ersten Mal
  - `cargo build -p pumpkin`: etwa 5 Minuten
  - `cargo test -p pumpkin-core`: deutlich länger
  - Beim ersten Build lädt `pumpkin-data` den Vanilla-Datapack herunter.
- **In Tests wirkt kein Schaden:** Ohne `Server` bricht `LivingEntity::damage_with_context` bei `world.server.upgrade()` mit `false` ab. Schaden lässt sich in Unit-Tests also nicht prüfen.
- **Mobs ticken nur in aktiven Chunks:** Ohne Spieler lädt `/forceload` keine Chunks, also bewegen sich Mobs im reinen Konsolen-Boottest nicht. Verhalten muss im Spiel getestet werden. Kein Bot unterstützt bisher 26.3.
- **Tick-Reihenfolge eines Mobs (`Mob::tick`):** `mob_tick` (vor dem Living-Tick, entspricht dem Anfang von Javas `tick()`), dann `living_entity.tick`, dann `post_tick` (danach). KI-Ziele laufen in `server_ai_step`: erst Zielauswahl, dann Ziele, Navigation, `custom_server_ai_step`, Move-, Look- und Jump-Control.
- **Muster aus dem Vex** (`entity/mob/vex.rs`), zum Abschauen:
  - **Eigene Move-Control:** `*mob_entity.move_control.lock() = Box::new(...)`
  - **Ziele mit Mob-Zustand:** Die Ziele halten `Weak<MobTyp>`, kein `Arc`, sonst entsteht ein Referenzzyklus.
  - **Besitzer:** `projectile::owner::EntityReference`
  - **Ausrüstung beim Spawnen:** `finalize_spawn` überschreiben, darin `populate_default_equipment_slots` und `populate_default_equipment_enchantments` aufrufen, dann `finalize_spawn_super`. Fallchance setzen mit `MobEntity::set_drop_chance`.
  - **Blockeffekte trotz `no_physics`:** `Mob::mob_is_affected_by_blocks` überschreiben.
- **`has_audited_tick` bleibt `false`,** solange niemand den ganzen Mob für die Entity-Islands geprüft hat. Neue Ziele geben in `tick_reach` `None` zurück. Das ist sicher, nur langsamer.
- **Upstream nachziehen:** `git remote add upstream https://github.com/Pumpkin-MC/Pumpkin` (falls noch nicht vorhanden), dann regelmäßig `git fetch upstream master` und in `master` mergen, danach `playable` aktualisieren. Je länger man wartet, desto mehr Konflikte gibt es.
