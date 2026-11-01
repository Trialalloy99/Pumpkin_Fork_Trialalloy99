# Auftrag für Codex: Batch 5 (10 Entities)

Du arbeitest im Fork `Trialalloy99/Pumpkin_Fork_Trialalloy99` von Pumpkin (Minecraft-Server in Rust). Parallel arbeitet ein anderer Agent (Claude) an Batch 4 auf dem Branch `batch-4`. Deine Aufgabe ist Batch 5.

## 1. Zuerst lesen (in dieser Reihenfolge)

1. `FORK.md`: Ziel, Arbeitsablauf, Regeln, Stand, Must-know. Das hat Vorrang vor allem anderen.
2. `AGENTS.md`: Projektregeln, Vanilla-Parität, Codequalität, Checks.
3. `CONTRIBUTING.md`, Abschnitt „Mob AI“: Ziele, Entity-Islands und `tick_reach`.
4. Als Vorbilder für Mobs, die schon nach Vanilla portiert sind:
   - `crates/pumpkin-core/src/entity/mob/creeper.rs`
   - `crates/pumpkin-core/src/entity/mob/skeleton/`
   - `crates/pumpkin-core/src/entity/mob/zombie/drowned.rs`
   - `crates/pumpkin-core/src/entity/mob/witch.rs`
5. Allgemeine Mob-Hooks im Trait `Mob` in `crates/pumpkin-core/src/entity/mob/mod.rs`:
   - Spawn und Ausrüstung: `finalize_spawn`, `populate_default_equipment_slots`
   - Ticks: `mob_tick`, `post_tick`, `custom_server_ai_step`
   - Kampf und Schaden: `pre_attack`, `on_attack`, `blocked_by_item`, `killed_entity`, `actually_hurt_override`, `hurt_armor`
   - Bewegung: `max_fall_distance`, `climbable_override`, `travel_in_water`, `on_cause_fall_damage`
   - Loot: `mob_wants_to_pick_up`, `preferred_weapon_type`, `keeps_equipped_item`
   - Sonstiges: `charge_speed_modifier`

   Daneben gibt es `SpawnReason` in `mob/spawn.rs` und `set_pending_vehicle`/`add_pending_rider` für Jockeys.

## 2. Deine 10 Entities

1. Slime
2. Magma Cube
3. Phantom
4. Silverfish
5. Blaze
6. Pillager
7. Piglin Brute
8. Hoglin
9. Zoglin
10. Polar Bear

Die Dateien liegen unter `crates/pumpkin-core/src/entity/mob/` bzw. `entity/passive/polar_bear.rs`. Alle existieren schon, sind aber nur teilweise fertig.

**Ziel:** Jedes Entity verhält sich wie in Vanilla 26.3. Ein Entity gilt erst als fertig, wenn es keine bekannte Lücke mehr hat (siehe FORK.md). Lücken schließt du in derselben Batch. Wenn dafür etwas Allgemeines fehlt (z. B. ein Hook, eine Navigation, ein Item-Effekt), baust du es ein. Lücken notierst du nicht als „später“.

## 3. Vanilla-Code holen

- **Server-JAR:** Lade das Server-JAR 26.3 über `https://piston-meta.mojang.com/mc/game/version_manifest_v2.json`. Das echte JAR liegt im Bundler unter `META-INF/versions/26.3/server-26.3.jar`.
- **Dekompilieren:** Nutze Vineflower und nur das, was du brauchst. Beispiel:

  ```
  java -jar vineflower.jar -only=net/minecraft/world/entity/monster/Slime <jar> out/
  ```

  Die Klassen sind nicht obfuskiert. Alles bleibt außerhalb des Repos.
- **Aufrufkette:** Folge der ganzen Kette: Superklassen, Goals, MoveControl, Navigation, `finalizeSpawn`, `hurtServer`, NBT, Sounds und Spawn-Regeln (`entity/type.rs`, `check_spawn_rules`).

## 4. Regeln, die du einhalten musst

- **Branch:** Zweige `batch-5-codex` von `batch-4` ab und pushe nur dorthin. Arbeite nie auf `batch-4`, dort arbeitet Claude.
- **Commits:**
  - Ein Commit pro Mob, im Format `feat(entity): port <mob> ...`.
  - Autor ist `Trialalloy99 <maxi.ratnik@gmail.com>`. Keine Co-Authored-By- oder Tool-Zeilen.
  - Keine Pull Requests.
- **Kommentare:** Neuer Code bekommt nur kurze englische Stichwort-Kommentare mit dem Vanilla-Namen, z. B. `// slime: split on death (Slime.remove)`. Kein Fließtext.
- **Performance:**
  - Keine Allokationen pro Tick, wenn es sich vermeiden lässt.
  - Billige Prüfungen (Random, Timer) zuerst.
  - Locks kurz halten, `Weak` statt `Arc` in Goals und Controls (sonst Speicherleck).
- **Generierte Dateien:** `crates/pumpkin-data/src/generated/` nie von Hand ändern. Fehlt ein Wert, erweitere `tools/pumpkin-codegen` und regeneriere mit `cargo run --locked -p pumpkin-codegen -- <datei>.rs`.
- **Entity-Islands:** `has_audited_tick` bleibt `false`. Neue Goals geben in `tick_reach` `None` zurück.
- **Konflikte mit Claude vermeiden:**
  - Bei geteilten Dateien (`entity/mob/mod.rs`, `entity/living.rs`, `entity/mod.rs`, `world/mod.rs`) nur kleine, additive Änderungen, neue Trait-Methoden mit Default.
  - Nichts umbenennen oder umstrukturieren.
  - Fass die Batch-4-Mobs nicht an: Skelett-Familie, Creeper, Spinnen, Drowned, Witch, Kamel und Camel Husk.

## 5. Ablauf und Tests

- **Pro Mob:**
  1. Vanilla lesen, portieren.
  2. `cargo check -p pumpkin-core`.
  3. Commit.
- **Am Ende der Batch:**
  1. `cargo fmt`
  2. `cargo clippy -p pumpkin-core --all-targets --all-features`: keine neuen Warnungen, Clippy ist pedantisch.
  3. `cargo test -p pumpkin-core`. Bekannt und egal: In `pumpkin-world` schlägt `world_info_save` fehl, wenn als root ausgeführt.
  4. Boottest:
     1. `cargo build -p pumpkin`.
     2. Server aus einem Scratch-Ordner starten (`<repo>/target/debug/pumpkin`).
     3. Warten bis „Server is now running“.
     4. Über stdin `summon minecraft:<mob> 0 70 0` für alle 10 Mobs und ein paar `data get entity @e[type=minecraft:<mob>,limit=1] <Feld>`.
     5. `stop`.
     6. Log nach panic, ERROR und WARN durchsuchen.

  Mobs ticken ohne Spieler nicht, Verhalten wird im Spiel getestet.
- **Plattenplatz:** Falls die Platte voll ist: `rm -rf target/debug/incremental target/release`.
- **Zum Schluss:**
  - Eine Checkliste für den Test im Spiel: „mach X, erwarte Y“ pro Mob.
  - In FORK.md unter „Stand“ eine Zeile für Batch 5 eintragen.
  - Pushen.

## 6. Tracker-Seite aktualisieren

Der Tracker ist ein claude.ai-Artifact: https://claude.ai/artifact/KAHK4pXmbbEhd9nXiBuobN. Seine Daten liegen in einer Datenbank, die nur über die Seite selbst oder über Claude geschrieben werden kann. Du (Codex) kannst sie nicht direkt ändern.

So läuft es:
1. Lege am Ende die Datei `TRACKER_BATCH5.json` im Repo-Root an. Pro Entity ein Eintrag:

   ```json
   [
     {"cat": "entities", "id": "slime", "status": "done", "batch": 5, "note": "kurze Notiz, was portiert wurde"}
   ]
   ```

   - `id` ist der Vanilla-Name in Kleinbuchstaben, z. B. `magma_cube`, `piglin_brute`, `polar_bear`.
   - `status` ist `done` nur ohne bekannte Lücke, sonst `wip` mit der Lücke in `note`.
2. Committe die Datei mit.
3. Der Besitzer oder Claude überträgt die Einträge dann in den Tracker. Claude schreibt sie über das Datenbank-Tool, Doc-ID `entities:<id>`. Der Besitzer kann sie auch direkt auf der Seite über die Auswahlfelder „Status“, „Batch“ und „Notiz“ eintragen.

## 7. Abschlussbericht (auf Deutsch)

- Was pro Mob portiert wurde.
- Welche allgemeinen Systeme neu dazukamen.
- Testergebnisse: fmt, clippy, test, Boottest.
- Die Checkliste für den Test im Spiel und was aufgenommen werden soll.
- Probleme, die du nicht lösen konntest, mit Grund.
