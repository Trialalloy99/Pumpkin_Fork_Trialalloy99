# FORK.md: zuerst lesen

Diese Datei gilt für alle Agents, die in diesem Fork arbeiten. Lies sie **vor** `AGENTS.md`, `CONTRIBUTING.md` und allen anderen `.md`-Dateien. Bei Widersprüchen gilt die Reihenfolge:

1. Was der Besitzer des Forks (Trialalloy99) dir direkt sagt
2. Diese Datei
3. `AGENTS.md` und die übrigen Projektdateien

Eine Regel aus `AGENTS.md` bleibt immer bestehen: Agents öffnen nie selbst Pull Requests und posten nichts auf GitHub.

## Ziel

Der Besitzer will so schnell wie möglich eine **spielbare Version von Pumpkin** für sich und seine Freunde. Der Fork ist `Trialalloy99/Pumpkin_Fork_Trialalloy99`, das Original ist `Pumpkin-MC/Pumpkin`. Wir bauen fehlende Entities und Items selbst ein, Batch für Batch. PRs ans Original kommen vielleicht später, aber nicht als nächster Schritt. Arbeite trotzdem so, dass jeder Commit später als PR taugen würde.

Tempo ist wichtig. Ein einfacher Mob wie der Vex hat etwa 10 Minuten Coding gebraucht, mit allen Checks knapp eine Stunde. Plane danach und verliere dich nicht in Nebenbaustellen.

## Performance zuerst

Pumpkin ist auf Geschwindigkeit ausgelegt. Das gilt für jede Zeile, die wir schreiben:
- **Mob-Ticks:** Mob-Code läuft pro Mob und Tick, also tausendfach pro Sekunde. Darin keine Allokationen (`Vec`, `String`, `format!`), kein `clone()` großer Daten und keine Suche über alle Entities ohne Bounding Box.
- **Billige Prüfungen zuerst:** Zufallswürfe, Timer und Flags vor Weltabfragen, Entity-Suchen und Pathfinding. Vanilla prüft oft in derselben Reihenfolge. Wo Vanilla die Reihenfolge vorgibt (z. B. beim Zufall), gilt Vanilla.
- **Locks kurz halten:** Kein Lock über einen Aufruf halten, der selbst sperrt. Kein Lock über `.await`. Atomics statt Mutex für einfache Zahlen und Flags.
- **Schwere Arbeit:** Chunk-Scans und ähnliches erst mit der Palette prüfen (`contains_matching`), wie bei den POIs. CPU-lastiges gehört auf Rayon, Datei-Arbeit auf `spawn_blocking`.
- **Referenzen:** `Weak` statt `Arc` in Zielen und Controls, damit keine Zyklen entstehen und Mobs freigegeben werden.
- **Ergebnisse merken:** Was teuer ist und sich nicht ändert, cachen. Vanilla cached zum Beispiel Struktur-Starts bei `/locate`.

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
- **Code:** Der Code soll aussehen wie der Rest des Projekts. Keine erklärenden Kommentare.
- **Such-Kommentare:** Neuer Code bekommt kurze englische Stichwort-Kommentare, damit künftige Agents die richtige Stelle mit wenig Suchen und wenig Tokens finden. Eine Zeile, nur Stichworte und der Vanilla-Name, z. B. `// vex: charge attack goal (Vex.VexChargeAttackGoal)` oder `// guardian: beam attack, 80 tick warmup`. Kein Fließtext, keine Erklärung, was die nächste Zeile tut.
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
- **Wann:** Beim Start einer Batch die Einträge auf `wip` setzen. Vorher lesen und `if_version` mitgeben.
- **Erst fertig, wenn es wirklich fertig ist:** `done` (im Tracker, in der Stand-Tabelle und im Bericht an den Besitzer) nur, wenn **alle Lücken geschlossen sind und sich das Ding wie in Vanilla verhält**. Bekannte Lücken werden in derselben Batch geschlossen, nicht als „später“ notiert. Solange eine Lücke offen ist, bleibt der Eintrag auf `wip`.
- **Offizielle Daten erneuern:** Die Einträge kommen aus https://github.com/Pumpkin-MC/Pumpkin-Website/blob/master/src/pages/tracker/data.json. Bei Bedarf neu einbetten und die Seite unter derselben URL neu veröffentlichen.

## Stand

| Batch | Inhalt | Stand |
|---|---|---|
| 1 | Vex (plus die Vex-Beschwörung beim Evoker) | fertig auf `feat/vex-ai` und `playable`; Test im Spiel steht aus |
| Commands | `/locate` (alle POI-Typen, POIs aus geladenen Chunks), `/perf` | fertig, in `playable` gemergt |
| 2 | Guardian, Elder Guardian, Illusioner, Vindicator, Ravager, Endermite | fertig, in `playable` gemergt; Test im Spiel steht aus |
| 3 | Zombie, Husk, Zombie Villager, Zombified Piglin, Enderman, Wolf | fertig, in `playable` gemergt (Husk erst mit dem Kamel-Husk aus Batch 4 komplett); Test im Spiel steht aus |
| 5 (Codex) | Slime, Magma Cube, Phantom, Silverfish, Blaze, Pillager, Piglin Brute, Hoglin, Zoglin, Polar Bear | in `playable` gemergt; Vanilla-Review und Test im Spiel stehen aus |
| 4 | Parched, Skeleton, Stray, Bogged, Creeper, Spider, Cave Spider, Drowned, Witch | fertig, in `playable` gemergt; Test im Spiel steht aus. Camel Husk verschoben (braucht das ganze Kamel) |
| 6 | Cow, Mooshroom, Pig, Sheep, Chicken, Rabbit, Iron Golem, Snow Golem, Squid, Glow Squid | Branch `batch-6`; Iron Golem bis auf „Villager will Golem“ (siehe unten); Test im Spiel steht aus |
| 7 | Items (Mace, Angel, Elytra, Namensschild), Redstone (Druckplatten, Tageslichtsensor, Notenblock, Kolben, Leitung), Welt laden/speichern, alle 22 Boote, Dreizack-Loyalität, Bugfixes aus dem Spieltest | Branch `batch-7`; Test im Spiel steht aus |
| Blöcke 1 (Gemini) | Bed, Beehive, Campfire, Chest, Chiseled Bookshelf, Conduit, Dirt Path, Dragon Egg, Dried Ghast, Ender Chest | in `playable` gemergt; Beehive offen (Bienen-Bewohner fehlen) |
| Blöcke 2 (Gemini) | Farmland, Frogspawn, Huge Mushroom, Infested, Jukebox, Lodestone, Potent Sulfur, Fire, Sculk Catalyst, Big Dripleaf | in `playable` gemergt; Sculk Catalyst offen (SculkSpreader fehlt) |
| 8 (Gemini) | Horse, Donkey, Mule, Skeleton Horse, Zombie Horse, Llama, Trader Llama, Goat, Armadillo, Strider (AbstractHorse-Basis) | in `playable` gemergt; Test im Spiel steht aus |
| 9 (Gemini) | Camel, Camel Husk, Cod, Salmon, Tropical Fish, Pufferfish, Dolphin, Panda, Nautilus, Zombie Nautilus | in `playable` gemergt; Panda und Delfin-Schatzsuche von Claude nachportiert; Test im Spiel steht aus |
| Redstone (Abschluss) | Druckplatten, Tageslichtsensor, Notenblock und Kolben gegen Vanilla geprüft; Kolben: bewegte Blöcke haben Kollision, Entities werden kollisionsbewusst geschoben, Pushreaction-Ausnahmen, Wasser/Zerstörung beim Landen | in `playable`; Test im Spiel steht aus |
| Performance | `/recording`, Himmelslicht, Jigsaw-Cache, Mth.floor, Gen-Threads, mimalloc, FxHash-Strukturcache | in `playable`; siehe unten |
| 10 | Breeze, Wither Skeleton, Creaking, Allay, Copper Golem, Happy Ghast, Sniffer, Wandering Trader, Piglin, Sulfur Cube | Branch `batch-10`; im Tracker auf `done`; Test im Spiel steht aus |
| 13 | Ender Drache samt Kampf, Elytra und Raketen, Warden, Wither, Drachenfeuerball, Erfahrungsflasche, Angelhaken, Villager (Brain, Tagesplan, POI-Tickets, Glocke), Phantom samt Spawner, Mannequin, Befehlsblock- und Spawner-Loren | in `playable`; Test im Spiel steht aus |

Aus dem Tracker entfernt, weil Vanilla sie auf einem dedizierten Server nicht hat: `/publish` und `/unpublish` (nur Einzelspieler) sowie `/jfr` (Java Flight Recorder, braucht eine JVM).

Batch 2 und 3: alle bekannten Lücken sind geschlossen. Was dafür neu dazukam (für spätere Agents):
- `mob/spawn.rs`: `SpawnReason` (Vanilla `EntitySpawnReason`), `finalize_spawn(.., reason)`, `MobEntity::spawn_reason()`, `set_pending_vehicle` (Mob reitet nach dem Spawnen auf, z. B. Chicken Jockey).
- `item/items/spear.rs`: `SpearItem::mob_kinetic_tick` (Speer für Mobs), `ai/goal/spear_use.rs` (`SpearUseGoal`).
- `world/village.rs`: `sections_to_village`, `is_close_to_village`, `is_village`, `find_occupied_village_poi`; `ai/goal/move_through_village.rs`.
- `Mob`-Hooks: `pre_attack` (jeder Nahkampfversuch), `blocked_by_item` (Schild hat geblockt), `actually_hurt_override` und `hurt_armor` (Wolfsrüstung), `charge_speed_modifier`.
- Schilde blocken jetzt auch Nahkampf ohne übergebene Position (vorher blockten sie Mob- und Spieler-Nahkampf gar nicht).
- Rüstung im Körper-Slot zählt beim Rüstungswert mit (Wolfs- und Pferderüstung).
- `LookAtEntityGoal::any_mob` (Vanilla `LookAtPlayerGoal(Mob.class)`) und `::matching` (mehrere Typen).
- `TrackTargetGoal`: Erreichbarkeit (`canReach`) und Heimat-Bereich sind umgesetzt.
- Bewusste Näherungen: Glocken gelten als „belegt“, sobald ein Villager in 48 Blöcken ist (Pumpkin-Villager beanspruchen keine Treffpunkte). `noCollision` beim Kamel-Husk prüft nur Blöcke.
- Technische Schuld ohne Verhaltensunterschied: Der Evoker nutzt noch sein eigenes Zaubersystem statt `mob/spellcaster.rs`.

Bekannt und nicht von uns: `cargo test -p pumpkin-world` hat einen Fehlschlag in `world_info_save` (`write_world_info_reports_final_rename_failure`), weil der Container als root läuft und Ordnerrechte nicht greifen.

Batch 4, neu dazugekommen:
- `mob/looting.rs`: Mobs heben Items auf und rüsten sie aus (Vanilla `Mob.aiStep` looting, `equipItemIfPossible`).
- Mobs klettern an Leitern, Ranken usw. (`LivingEntity::check_climbing`, vorher ein TODO).
- `Mob::max_fall_distance` geht in die Navigation (Vanilla `getMaxFallDistance`).
- Monster laufen lieber ins Dunkle (`get_walk_target_value`).
- Explosionen kennen ihren Verursacher (`World::explode_from_entity`).
- `Mob::killed_entity` kann Drops verhindern (Villager-Infektion jetzt wie Vanilla).
- `RestrictSunGoal` und `FleeSunGoal` wie Vanilla. Die Navigation meidet Sonne per `can_see_sky`.

Batch 6, neu dazugekommen:
- Reiten mit Steuerung: `Mob::tick_ridden`, ein Spieler als steuernder Passagier lässt den Client das Tier bewegen (Vanilla `travelRidden`). Der Schweine-Sattel liegt jetzt im SADDLE-Slot (Client sieht ihn), Boost wird an den Client synchronisiert (auch beim Strider).
- Eigene Sprung-Steuerung: `Mob::jump_control_tick`, `jump_power_multiplier`, `on_jump_from_ground` (Hase).
- `Mob::mob_attack_damage` (Schadenswurf, Eisengolem), `Mob::on_push_entity` (Vanilla `doPush`).
- `Animal::eating_sound` statt Ambient-Sound beim Füttern.
- `world/village.rs`: `occupied_village_sections` + `sections_to_village_in` (ein POI-Scan für viele Abfragen), `occupied_pois_in_range`.
- `land_random_pos::get_pos_towards`, `VillagerEntity::get_player_reputation`.
- Codegen: `Biome::snow_golem_melts` (heiße Biome schmelzen Schneegolems).

Batch 7, neu dazugekommen:
- **Regel:** Beim Portieren auch den mitbenutzten Code gegen Vanilla prüfen (z. B. den Pfeil beim Skelett), nicht nur die Klasse selbst.
- Projektile treffen Spieler (`get_all_at_box` statt `get_entities_at_box`), Projektile haben Wasser-Physik (`Entity::update_fluid_state` ist `pub(crate)`).
- `default_push` (Vanilla `Entity.push`) als freie Funktion; der Server schiebt Spieler nie (Client-seitig).
- Natürliches Spawnen: Gewichte aus den Biom-Daten (Codegen `Spawner::weight`), Himmelsverdunkelung beim Monster-Licht, Tier-Spawns beim Chunk-Generieren lesen Boden und Licht aus der Proto-Chunk.
- `World::is_handling_tick` (Vanilla `isHandlingTick`), `block::write_block_state_nbt` / `read_block_state_nbt` (BlockState.CODEC).
- `EntityBase::on_above_bubble_column` (Boote verzögern den Effekt).
- Kisten-Boote nutzen `minecart::container::MinecartInventory`.
- Weltdaten: Spielzeit, Tageszeit und Wetter werden geladen und gespeichert, `DataVersion` liegt in den `data/minecraft/*.dat` neben `data` (sonst kann Vanilla sie nicht lesen). Getestet per Rundlauf Vanilla → Pumpkin → Vanilla mit `bench/vanilla` (Java 25 liegt unter `scratchpad/bench/jre`).
- Wetterzyklus lief nie (Bedingung war umgedreht), jetzt mit Spielregel `advance_weather`.
- `PiglinAi::anger_nearby_piglins`: Container öffnen (Truhe, Fass, Shulker, Enderkiste, Kisten-Lore/-Boot), Kisten-Fahrzeug zerstören, `guarded_by_piglins` abbauen. Näherung: Pumpkins Piglin hat kein Brain, die Wut setzt das Angriffsziel statt `ANGRY_AT`.
- Leinen wie Vanilla `Leashable`: Feder-Physik mit Drehmoment (`Entity::tick_leash`, `leash_angular_momentum`), Quad-Leine Boot/Happy Ghast, `crate::entity::leash_interact` (Leine anlegen/lösen, Schere, Sneak-Übertragen) für Mobs und Boote.

Batch 10, neu dazugekommen:
- Brain-Bausteine in `ai/brain/behavior`: `MeleeAttack`, `SetWalkTargetFromAttackTargetIfTargetOutOfReach`, `InteractWithDoor`, `TransportItemsBetweenContainers`, `FollowTemptation`, `BabyFollowAdult`, `AnimalMakeLove`, `SetEntityLookTarget` (Typ, Predicate, beliebig), `CopyMemoryWithExpiry`, `EraseMemoryIf`, `GoToTargetLocation`, `SetWalkTargetAwayFrom`, `BackUpIfTooClose`, `InteractWith`, `SetLookAndInteract`, `CrossbowAttack`, `SpearApproach/Attack/Retreat`, `StartCelebratingIfTargetDead`, `StopBeingAngryIfTargetDead`. `OneShot::when` ist Vanilla `triggerIf`, `Behavior::stop_timed` meldet, ob die Zeit abgelaufen ist.
- Sensoren: `TemptingSensor` (`FoodTemptations`), `AdultSensorAnyType`, `PiglinSpecific`. `Mob::wants_to_pick_up_in_brain` lässt Sensoren das Brain mitgeben.
- Bewegung: `GhastMoveControl` (Happy Ghast), `MoveControlTrait::wanted_position/set_wait`, `MobEntity::stop_in_place`, `LivingEntity::travel_flying_with`, `TemptGoal::for_non_pathfinders`, `Entity::leash_snap_distance/leash_elastic_distance`, `Mob::on_elastic_leash_pull`, `Mob::mob_is_collidable`.
- Restitution (Blöcke und Entities), Sulfur-Cube-Archetypen per Codegen, Kupfergolem-Bauschema (`carved_pumpkin.rs`), `block/blocks/chests.rs::get_chest_container`.
- `WanderingTraderSpawner` (`world/wandering_trader_spawner.rs`), Timer in `wandering_trader.dat` über `LevelData`.
- Näherungen: Brain-Anger-Broadcasts der Piglins nutzen `try_with_brain` (ein gerade tickender Nachbar wird übersprungen). Der Piglin Brute bleibt goal-basiert. Im Kupfergolem fehlt das Aufsetzen einer Mohnblume (kein Equippable im Item-Datensatz). Der Creaking-Heart-Block erzeugt noch keine Creakings.

Performance (Chunk-Laden), neu dazugekommen:
- `/recording start|stop` (`chunk_system/profiler.rs`): misst jede Generierungsstufe, Disk-IO, Scheduler und Senden; Bericht in `recordings/chunks-*.txt`. Scheduler-Zeiten sind pro Dimension aufsummiert und daher nur grob.
- Himmelslicht: offener Himmel unbeleuchteter Nachbarn wird beim Ausbreiten übersprungen (22 → 4 ms/Chunk, Licht bit-gleich).
- `TemplatePool::get_max_size` gecacht (Vanilla `maxSize`), Noise nutzt `math::floor_i32` (Vanilla `Mth.floor`), Gen-Pool = Kerne − 1 (Vanilla), mimalloc als Allocator, Strukturstart-Cache mit FxHash.
- Windows-exe mit `RUSTFLAGS="-C target-cpu=x86-64-v3"` bauen (`znver5` lässt LLVM abstürzen).
- Offen: `gen: surface` kostet auf Windows mit 31 Threads ~20 ms/Chunk (lokal 6 ms); `gen: surface setup` in der nächsten Aufnahme zeigt, ob Router/Erzadern oder die Spalten-Regeln schuld sind.

Tracker: Der Tracker liegt jetzt auf https://tracker.trialalloy.eu (API `PUT /api/progress/<kat>:<id>` mit Header `X-Tracker-Password`, Passwort beim Besitzer).

Offene Aufgaben, die nicht Teil einer Batch waren:
- **Kamel und Camel Husk:** Vanilla nutzt Brain-KI (CamelAi), Sitzen und Dash, Reiten und die ganze `AbstractHorse`-Basis (Zähmen, Sattel, Inventar). Das fehlt in Pumpkin. Erst danach funktioniert der Kamel-Husk-Jockey beim Husk. Der Code dafür ist schon drin und springt an, sobald `CAMEL_HUSK` ein Mob ist.
- **Zombie-Nautilus:** fehlt als Mob. Der Drowned-Jockey springt automatisch an, sobald er existiert.
- **`/effect`:** geht in Pumpkin nur für Spieler, in Vanilla für alle Lebewesen.
- **Villager-Gedächtnis:** Villager merken sich nicht, wann sie geschlafen haben (`LAST_SLEPT`) und ob ein Golem in der Nähe war (`GOLEM_DETECTED_RECENTLY`). Deshalb fehlt beim Eisengolem das Laufen zu Villagern, die einen Golem wollen, und das Golem-Spawnen der Villager ist nicht Vanilla.
- **Ertrinken an Land:** Landmobs verlieren unter Wasser keine Luft (nur Spieler und Wassertiere haben Luft-Logik).

Platz: `target/debug/incremental` wächst auf über 20 GB. Wenn die Platte voll ist, `rm -rf target/debug/incremental target/release`.

Offene Upstream-PRs sind für die Auswahl egal. Wir bauen unsere eigene spielbare Version und warten auf niemanden. Wichtig sind sie nur als Hinweis, dass es beim Nachziehen von upstream an diesen Stellen Konflikte geben kann. Upstream arbeiten andere bereits an: Silverfish (#3886), Phantom (#3715), Spider und Cave Spider (#4368), Witch (#4393), Pufferfish (#4050), Zoglin und Hoglin (#4095), Ausrüstung beim Spawnen (#4391). Den Endermite hat AdmerPRO beansprucht (#4255). 

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

## Speedtest (Commands-Batch, 2026-11-01)

Gemessen im Container mit 4 Kernen, Seed 12345, Sichtweite 10, ohne Spieler. Pumpkin als Release-Build mit Thin-LTO. Vanilla 26.3 und Paper 26.3 (Build 235) liefen mit Java 25 und `-Xms2G -Xmx4G`. `/locate` schreibt bei allen drei dieselbe Log-Zeile „Locating element … took … ms“.

| Messung | Pumpkin | Vanilla | Paper |
|---|---|---|---|
| Start bis bereit | 0,2 s* | 12,2 s | 15,0 s |
| RAM nach dem Start | 81 MB | 1,69 GB** | 2,11 GB** |
| `/locate structure village_plains` (erste Suche) | 801 ms | 287 ms | 246 ms |
| `/locate structure #village` (zweite Suche) | 1994 ms | 3 ms | 4 ms |
| `/locate structure stronghold` | 0 ms | 12 ms | 12 ms |
| `/locate biome cherry_grove` | 19 ms | 48 ms | 42 ms |
| `/locate biome mushroom_fields` | 484 ms | 792 ms | 763 ms |

\* Pumpkin generiert beim Start keinen Spawn-Bereich vor, Vanilla und Paper schon.
\** Bei Java ist der RAM-Wert stark durch `-Xms2G` bestimmt.

Was dabei aufgefallen ist (eigene Aufgaben, nicht in dieser Batch behoben):
- Strukturen: Vanilla merkt sich bereits gefundene Struktur-Starts (zweite Suche 3 ms), Pumpkin rechnet jedes Mal neu.
- `/locate structure mansion` findet in Pumpkin eine andere Position (12032, 5280) als Vanilla (4656, 7056). Bei Woodland Mansions stimmt also etwas an der Platzierung oder der Biom-Prüfung nicht.
- Die Abstände weichen um ein paar Blöcke ab (841 statt 835), weil die Konsole in Pumpkin von einer anderen Position aus sucht als vom Welt-Spawn wie in Vanilla.

Batch 13, neu dazugekommen:
- POI-Tickets: `PoiStorage::take_ticket/release_ticket/free_tickets`, `world/poi_manager.rs` (`poi_find_all_closest_first`, `poi_take`, `poi_release`, `poi_is_occupied`, `PoiOccupancy`). Betten, Glocken und Arbeitsplätze zählen Tickets wie Vanilla; `VillagerPoiStorage` gibt es nicht mehr.
- Brain: `BrainState::set_schedule/update_activity_from_schedule` (Tagesplan aus `World::villager_activity`), `PathNavigationTrait::create_path_to_targets`, `GlobalPos::of(world, pos)`, `Mob::mob_on_death`, `Mob::drop_custom_death_loot`.
- Villager: `entity/passive/villager/{ai,behaviors,routines,actions}.rs` und `ai/brain/sensing/villager.rs`. Ein anderer Villager wird nur über `try_with_brain` gelesen oder verändert; ist dessen Brain gerade beschäftigt, wird er übersprungen (Näherung wie beim Piglin).
- Glocke: `BellBlockEntity` setzt `HEARD_BELL_TIME` im Umkreis von 32 Blöcken, Resonanz und Leuchten der Plünderer wie Vanilla.
- Phantom: eigene `PhantomMoveControl` (Geschwindigkeit, Drehung, Neigung wie Vanilla), `PhantomLookControl` ohne Wirkung, `Mob::custom_body_rotation`, Zielbedingungen über `TargetPredicate`, Anker optional, NBT `anchor_pos`/`size` (alte `AX/AY/AZ/Size` werden noch gelesen), XP fest 5. Neu: `world/phantom_spawner.rs` (`PhantomSpawner`, Zeit seit dem letzten Schlafen, Schwierigkeit, Gamerule `spawn_phantoms`), davor spawnten Phantome gar nicht natürlich.
- Erfahrungsflasche: `ThrownItemEntity::set_velocity_from_owner` addiert die Bewegung des Werfers wie `Projectile.shootFromRotation`. Schneeball, Ei und Enderperle nutzen noch `set_velocity_from` ohne diesen Zusatz.
- Spawner-Lore: `world/base_spawner.rs` (`BaseSpawner`: Verzögerung, `SpawnData`, `SpawnPotentials`, `custom_spawn_rules`, Nachbarlimit, Spawn-Regeln). Der Spawner-Block (`MobSpawnerBlockEntity`) nutzt ihn noch nicht. `equipment` in `SpawnData` fehlt.
- Befehlsblock-Lore: `CommandSender::CommandMinecart`, Aktivierung nur auf eingeschalteter Aktivierungsschiene mit 4 Ticks Abstand, Paket `SetCommandMinecart` setzt Befehl und Ausgabe. Als Entität für `@s` kennt die Quelle die Lore nicht (`CommandSource` hält nur Spieler).
- Mannequin: `entity/decoration/mannequin.rs` (Profil, Ebenen, Hand, Pose, unbeweglich, Beschriftung samt NBT) und `MetadataSerializer` für `ProfileImpl`.
- Armadillo: Brain statt Goals (`passive/armadillo/ai.rs`: Core, Idle, Panic mit `ArmadilloBallUp`), neuer Sensor `ArmadilloScareDetected`, `NearestAdult`, `RandomLookAround`, `AnimalPanic::with_causes`, `AnimalMakeLove::with_close_enough_distance`, `mob/gift_loot.rs` (Loot-Tabellen `gameplay/armadillo_shed` und `brush/armadillo`). Scute-Zeit 6000 bis 12000 Ticks, Baby-Alter -24000 (vorher 24000 bis 48000 und -48000).
- Ziege: Brain statt Goals (`passive/goat/ai.rs`: Core, Idle, LongJump, Ram mit `RamTarget` und `PrepareRamNearestTarget`), neue allgemeine Behaviors `LongJumpToRandomPos` und `LongJumpMidJump` (`ai/brain/behavior/long_jump.rs`). Horn mit zufälligem Instrument, Wurfgeschwindigkeit, Schwere nach Alter, Fallschaden -10, Baby-Maße, Schrei-Chance über Eltern. Schildblock halbiert den Rückstoß, der Landefeld-Malus kommt aus dem Pfadtyp des Blocks, `ON_TOP_OF_POWDER_SNOW` ist hier `DangerPowderSnow`.
- Katze: Samthütten-Spawn nur schwarz, Fütterung heilt nach `nutrition`, Morgengeschenk mit `Entity::random_teleport` (Vanilla `randomTeleport`), `CatTemptGoal` mit gewähltem Spieler, der nie erschreckt (Ozelot: `OcelotTemptGoal`), `is_stepping_carefully` als Hook auf `EntityBase` (geduckte Katzen und Ozelots lösen keine Schritt-Vibrationen aus; Warden und Allay nutzen ihn).
- Axolotl: Brain statt Goals (`passive/axolotl/ai.rs`: Core, Idle, Fight, PlayDead mit `ValidatePlayDead`), Sensor `AxolotlAttackables`, neue Behaviors `TryFindLiquid`, `RandomStroll.swim` und `stroll_from_water`, Geschwindigkeit als Funktion bei `FollowTemptation`, `BabyFollowAdult`, `SetWalkTargetFromLookTarget` und `SetWalkTargetFromAttackTarget`, `goal_utils::for_each_by_manhattan`. Jagdpause ist das Gedächtnis `HAS_HUNTING_COOLDOWN` (im Eimer als Restzeit), Totstellen das Gedächtnis `PLAY_DEAD_TICKS`, Kopfdrehung 1 wie Vanilla.
- Frosch: Brain statt Goals (`passive/frog/ai.rs`: Core, Idle, Swim, LaySpawn, LongJump, Tongue mit `Croak` und `ShootTongue`), Sensoren `FrogAttackables` und `IsInWater`, Behaviors `TryFindLand`, `TryFindLandNearLiquid`, `TryLaySpawnOnFluidNearLand`, `LongJumpToRandomPos::with_preferred_block/with_landing_spot` (Vanilla `LongJumpToPreferredBlock`), `Navigator::frog()` (Eckenschneiden nie über Wasserrand, `FROG_PREFER_JUMP_TO` zählt als offen, Startknoten am Boxboden). Schwangerschaft ist das Gedächtnis `IS_PREGNANT`, nicht erreichbare Zungenziele `UNREACHABLE_TONGUE_TARGETS`; Kopfdrehung 35 und 5 wie Vanilla. `frog_goals.rs` ist weg.
- Biene: lockte nie Spieler mit Blumen an (leere Tempt-Liste), jetzt `TemptGoal::with_item_predicate` mit `BEE_FOOD`; Futter ist nur noch `BEE_FOOD` (kein geschlossener Eyeblossom). Neu: Stachelzähler am Opfer (`LivingEntity::stinger_count`, fällt nach `20 * (30 - n)` Ticks einzeln ab), `Navigator::bee_flying()` (Haltepunkt braucht Nicht-Luft darunter), `Entity::leash_offset_factors` (Leinenansatz wie `Bee.getLeashOffset`), Lautstärke 0,4 und Hurt/Death-Sound, `Mob::liquid_jump_boost` (Biene 0,01 statt 0,04), Pfadvergleich beim Stock-Zählen im Stockflug über `Path::same_as_path`.
- Fuchs gegen `Fox.java` geprüft: Konstruktor-Malus (`DangerOther`/`DamageOther` 0) und Pfadlänge 32, Leinenansatz (0,55 Augenhöhe, 0,4 Breite), `FoxFloatGoal` mit Schwelle 0,25 (`SwimGoal::with_fluid_threshold`), `FoxBreedGoal` leert auch den Partner (`BreedGoal::mate`), Alarm-Auswahl mit den Kampfbedingungen (12 Blöcke, Box um den Fuchs, schlafende Tiere zählen nicht), `PerchAndSearch` und `SearchForItems` fragen `getLastHurtByMob`, Kopf-Grenzen beim Umschauen, verkürzte Intervalle bei `SeekShelter` und Dorfspaziergang, Beeren per `drop_stack` samt Blockwechsel-Ereignis, Aufnahme-Animation, Schlaf- und Kreisch-Geräusch (`FOX_SLEEP`, `FOX_SCREECH` mit Lautstärke 2), schlafende Füchse ohne Eingabe. Spawn-Ei-Junge kommen aus `getBreedOffspring` (Fellfarbe der Eltern) und vertrauen dem Spawner (`Mob::on_offspring_spawned_from_egg`).
- Esel gegen `AbstractHorse`, `AbstractChestedHorse` und `Donkey` neu geschrieben (`passive/donkey.rs`): Ziele wie Vanilla (Schwimmen, Panik, `RunAroundLikeCrazy`, Zucht mit allen `AbstractHorse`-Typen über `BreedGoal::with_partner_types`, Locken, Folgen, wasservermeidendes Wandern, `RandomStandGoal`), Maultier-Zucht (`canMate` mit Esel und Pferd, `createOffspringAttribute` mit den Vanilla-Bereichen), `handleEating` mit `getAge() == 0` und nicht erzwungenem Altern, Interaktion in Vanilla-Reihenfolge (Baby oder Reiter geht an `Animal.mobInteract`, Leine über `animal_interact`), Aufbäumen mit 20-Tick-Zähler (vorher blieb ein wütender Esel für immer stehen), Zähmen synchronisiert das Flag, Maulzähler, Grasfressen, Heilen 1 von 900, Mutter folgen, `EatingHaystack`, Truhe samt Inhalt im NBT (`Items`) und als Drop beim Tod, Landegeräusch, Lautstärke 0,8, Ambient-Intervall 400, Schiebbarkeit, Attribute aus den Daten (Gesundheit wird erst beim Spawnen zufällig). `RunAroundLikeCrazyGoal` wirft Reiter ab, ruft `makeMad` und sendet Ereignis 6 wie Vanilla. Pferd, Maultier und die anderen Pferdeartigen nutzen weiter `abstract_horse.rs` und haben diese Korrekturen noch nicht.
- Pferdeartige: gemeinsamer `HorseLike`-Trait und Makro `horse_mob_hooks!` in `passive/abstract_horse.rs` (Aufbäumen mit Zähler, Zähmen mit Flag-Sync, `handleEating`, Grasfressen, Heilen, Mutter folgen, Galopp-Zähler, Sprungton, NBT `EatingHaystack`/`Bred`/`Temper`/`Tame`/`Owner`, Landegeräusch, Lautstärke 0,8, `RandomStandGoal`). Esel und Maultier sind jetzt dieselbe Entität (`MuleEntity` ist ein Alias, Töne nach Typ, Maultier paart nie). Pferd (Zucht mit allen Vanilla-Bereichen, Attribute erst beim Spawnen), Skelett- und Zombiepferd nutzen den Trait. Lama und Händlerlama bekamen nur die korrigierten Helfer (`tame_horse` synchronisiert das Flag, `throw_rider_off` spielt nur den Ton, weil Lamas nicht aufbäumen). Neu: `Entity::quad_leash_params` (Vierpunkt-Leine mit Vanilla-Werten für Pferde und Kamele), Passagiere erleiden den Fall des Reittiers (`Entity.propagateFallToPassengers`), `Mob::can_dispenser_equip_into_slot` beim Fuchs (nur Maul bei Aufheben erlaubt). Nicht gelöst: die zwei synchronisierten Vertrauens-Slots des Fuchses (der Client liest sie nicht, das Protokoll kennt den Metadatentyp nicht).
- Lama und Händlerlama sind jetzt eine Entität (`passive/llama.rs`, `TraderLlamaEntity` ist ein Alias) auf dem `HorseLike`-Gerüst: Vanilla-Zielreihenfolge, Pfadlänge 40, Leinenansatz, Max-Temper 30, `Llama.handleEating`, Truhenspalten nach Stärke samt Inhalt im NBT und als Drop, Zucht mit Stärke und Fell, Spucken mit `didSpit` (lässt das Rache-Ziel los), echte Karawane (`LlamaFollowCaravanGoal`: Kopf, Ende, Limit 8, Aufholen bis Tempo 3), Händlerlama: Verteidigen des Händlers, Despawn mit `ageLocked`, kein Aufsitzen an der Händlerleine.
