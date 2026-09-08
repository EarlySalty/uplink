# Hochkant-Editor: Speicher- und Laufzeitvertrag

Stand: 9. September 2026. Gilt für die Dashboard-Komponente `UplinkHochkantEditor` (Deadlock-Twitch-Bot, `bot/dashboard_v2/src/components/uplink/`) und die dafür nötigen Uplink-Bausteine. Bestandsaufnahme vorab: der Filtergraph für Ausschnitt, gestapelt und Bild-im-Bild existiert in `crates/uplink-media/src/graph.rs` (`EncodeProfile::filter`, `validate_layout_dimensions`) und wird nicht neu gebaut; `LayoutRevision` in `crates/uplink-core/src/model.rs` trägt die Revision ohne Geometrie; `LayoutSpec`/`Composition` in `crates/uplink-media/src/lib.rs` tragen die Geometrie in Quellpixeln zur Laufzeit. Neu sind das persistente, normierte Layoutformat, die Speicherroute, die Umrechnung auf `LayoutSpec` beim Sessionstart und die Vorschauquelle.

Der Social-Media-Layouteditor (`components/socialmedia/LayoutEditor.tsx`, `utils/socialMediaLayout.ts`) bleibt unangetastet: er rechnet in Pixeln zweier fester Räume (Quelle und 1080×1920) und hängt an Clip-Standbildern. Der Produktvertrag verlangt für Uplink normierte Koordinaten, Kamera an/aus, Bild-im-Bild und gestapelt, Seitenverhältnisprüfung, Live-Vorschau, versionierte Speicherung und einen definierten Übernahmezeitpunkt (`docs/produktvertrag-v0.3.md`, P-13 und Abschnitt Layouteditor).

## Layoutformat (normiert, versioniert)

```ts
export interface HochkantRahmen { x: number; y: number; w: number; h: number }   // 0..1, relativ zur jeweiligen Fläche

export type HochkantModus = 'nur_gameplay' | 'gestapelt' | 'bild_im_bild';
export type HochkantBandLage = 'oben' | 'unten';

export interface HochkantLayout {
  version: 1;
  modus: HochkantModus;
  gameplay: HochkantRahmen;                 // Ausschnitt aus der Quelle
  kamera: HochkantRahmen | null;            // Ausschnitt aus der Quelle, null = Kamera aus
  kameraBand: { hoehe: number; lage: HochkantBandLage } | null;   // nur gestapelt, hoehe 0..1 des Zielbilds
  kameraBox: HochkantRahmen | null;         // nur bild_im_bild, relativ zum Zielbild
}
```

Regeln, die Editor und Dienst gleich prüfen:

1. Alle Rahmen liegen vollständig in `[0, 1]`, jede Kante ist mindestens `0.05`.
2. Seitenverhältnis wird im Editor gesperrt, damit der Dienst nie mit schwarzen Balken auffüllen muss: `nur_gameplay` und `bild_im_bild` schneiden das Gameplay im Zielverhältnis (Standard 9:16); `gestapelt` schneidet das Gameplay im Verhältnis `zielBreite : (zielHoehe × (1 − kameraBand.hoehe))` und die Kamera im Verhältnis `zielBreite : (zielHoehe × kameraBand.hoehe)`; bei `bild_im_bild` hat `kameraBox` dasselbe Verhältnis wie `kamera`.
3. `kamera = null` erzwingt `modus = 'nur_gameplay'`. `kameraBand` ist nur bei `gestapelt`, `kameraBox` nur bei `bild_im_bild` gesetzt; `kameraBand.hoehe` liegt in `[0.1, 0.5]`.
4. `version` ist die Formatversion des JSON, nicht die Revision. Die Revision vergibt der Dienst beim Speichern.

Anfangswert ohne gespeichertes Layout: `bild_im_bild`, Gameplay mittig im Zielverhältnis aus der Quelle, Kamera `{x: 0.78, y: 0.05, w: 0.2, h: 0.2 × quelleSeitenverhältnis}` (rechts oben), `kameraBox` rechts oben mit Breite `0.35` des Zielbilds.

## Umrechnung zur Laufzeit (Uplink, beim Sessionstart)

Eingaben: gemessene Quelle `(qb, qh)` aus der Medienvorprüfung, Zielprofil `(zb, zh)` des Hochkantausgangs, gespeichertes Layout. Ausgabe: `LayoutSpec { revision, composition }` aus `uplink-media`.

| Modus | `Composition` | Pixelrechnung |
| --- | --- | --- |
| `nur_gameplay` | `Crop(gameplay)` | `x = gerade(round(gameplay.x × qb))`, ebenso `y`, `w`, `h`; Untergrenze 2 |
| `gestapelt` | `Stacked { gameplay, camera, camera_height }` | `camera_height = gerade(round(kameraBand.hoehe × zh))`, muss `< zh − 2` sein |
| `bild_im_bild` | `PictureInPicture { gameplay, camera, camera_box }` | `camera_box` aus `kameraBox × (zb, zh)`, gerade, vollständig im Zielbild |

`gerade(n)` rundet auf die nächste gerade Zahl ab; Größen (`w`, `h`, `camera_height`) haben die Untergrenze 2, Positionen (`x`, `y`) werden nach dem Runden so geklemmt, dass `x + w` und `y + h` in der jeweiligen Fläche bleiben (erst die Position zurückschieben, sonst die Größe um 2 verkleinern). Editor und Dienst rechnen identisch; der Editor sperrt das Speichern, wenn ein Rahmen nach der Pixelrechnung übersteht. `kameraBand.lage = 'oben'` bietet der Editor erst an, wenn der Graph die umgekehrte `vstack`-Reihenfolge kann; bis dahin lehnt auch die Editorprüfung `oben` ab. Nach der Rechnung läuft `validate_layout_dimensions` aus dem Graphen; ein Fehler dort ist ein sichtbarer Zielfehler (`output_state=failed`, `reason` mit Klartext), nie ein stilles Weglassen des Layouts. `Stacked` stapelt im heutigen Graphen Gameplay oben und Kamera unten (`vstack` in Reihenfolge Gameplay, Kamera); `kameraBand.lage = 'oben'` braucht die umgekehrte Reihenfolge im Filter, eine Zeile in `graph.rs`, die Codex mit übernimmt. Bis dahin lehnt der Dienst `lage = 'oben'` mit Klartext ab statt still `unten` zu liefern.

`LayoutRevision { id, revision }`: `id` ist die Layoutzeilen-ID (`relay.hochkant_layouts.layout_id`), `revision` die Revisionsnummer. Beide ungleich 0, wie der Planer es verlangt.

## Persistenz (additiv, Uplink)

```sql
CREATE TABLE IF NOT EXISTS relay.hochkant_layouts (
    layout_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    streamer_id bigint NOT NULL REFERENCES relay.users(streamer_id),
    revision bigint NOT NULL CHECK (revision > 0),
    layout jsonb NOT NULL,
    quelle_breite integer, quelle_hoehe integer,            -- Annahme beim Bearbeiten, nur Anzeige
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (streamer_id, revision)
);
ALTER TABLE relay.destinations ADD COLUMN IF NOT EXISTS hochkant_enabled boolean NOT NULL DEFAULT false;
ALTER TABLE relay.destinations ADD COLUMN IF NOT EXISTS hochkant_width integer;
ALTER TABLE relay.destinations ADD COLUMN IF NOT EXISTS hochkant_height integer;
```

Revisionen sind unveränderlich; Speichern legt immer eine neue Zeile mit `revision = max + 1` je Streamer an. Es gibt keine Löschung, alte Revisionen bleiben für laufende Sessions und Nachvollziehbarkeit erhalten. `hochkant_enabled` je Ziel bedeutet: dieses Ziel bekommt zusätzlich zum Querformat den Hochkantausgang (`canvas_index = 1`) mit dem Profil `hochkant_width × hochkant_height` (Anfangswert 1080×1920). Ein Ziel, dessen Plattform nur Hochkant annimmt (TikTok), setzt `hochkant_enabled` und trägt kein Querformatprofil. Die Grenze `UNIQUE (streamer_id, platform)` in `relay.destinations` bleibt: Hochkant ist ein zweiter Canvas desselben Ziels, kein zweites Ziel.

## Übernahmezeitpunkt

Beim Sessionstart liest der Dienst die höchste Revision des Streamers, friert sie für die Session ein und meldet sie als `active_revision` je Ziel. Ein Speichern während der Session erzeugt eine neue Revision, die erst die nächste Session nutzt; das Dashboard zeigt `requested_revision` (höchste) gegen `active_revision` (eingefroren), analog zu `requested` gegen `active_profile` bei den Profilen. Ein Wechsel während des Streams ist nicht Teil dieses Vertrags (`capabilities.layout_live = false`); er hängt an derselben Frage wie F5 und braucht die Entscheidung des Nutzers.

## Routen (Uplink, Vorschlag für Codex)

| Route | Vertrag |
| --- | --- |
| `GET /v1/me/hochkant-layout?streamer_id=…` | `{ revision, layout, quelle: {breite, hoehe} \| null, gespeichert_at, ziele: [{ platform, hochkant_enabled, requested_revision, active_revision, hochkant_profile }] }`; ohne gespeichertes Layout `revision = 0, layout = null` |
| `PUT /v1/me/hochkant-layout` | `{ streamer_id, layout, quelle? }`; validiert nach den Regeln oben, legt Revision `max + 1` an, Antwort wie GET |
| `PUT /v1/me/destinations` | zusätzliche optionale Felder je Ziel: `hochkant_enabled`, `hochkant_width`, `hochkant_height`; ausgelassene Felder bleiben erhalten |
| `GET /v1/me/standbild.jpg?streamer_id=…` | letztes Standbild der laufenden Session aus dem Decoder (höchstens ein Bild je 10 Sekunden, JPEG, längste Kante 960 Pixel, im Vorschaubudget des Produktvertrags); 404 ohne Session |

`capabilities.layout` wird erst `true`, wenn Speicherroute, Sessionstart-Übernahme und mindestens ein Ziel mit `hochkant_enabled` nachgewiesen sind. Bot-Routen dazwischen: `GET/PUT /twitch/api/v2/uplink/hochkant-layout` und `GET /twitch/api/v2/uplink/standbild.jpg`, jeweils mit Session-Identität, die der Bot in `streamer_id` übersetzt.

## Vorschauquelle

Die Komponente erhält `standbildUrl` als Prop und kennt keine Quelle selbst. Reihenfolge für den Aufrufer: (1) Uplink-Standbild der laufenden Session; (2) außerhalb einer Uplink-Session das öffentliche Twitch-Vorschaubild `https://static-cdn.jtvnw.net/previews-ttv/live_user_<login>-1920x1080.jpg`, solange der Kanal live ist (der Avatar-Host ist in der Dashboard-CSP bereits erlaubt); (3) sonst ein Raster ohne Bild, die Rahmen bleiben bedienbar. Ein Standbild ist eine Bedienhilfe, keine Zusage über das spätere Bild; das Dashboard schreibt das dazu.

## Komponente `UplinkHochkantEditor`

```ts
export interface HochkantQuelle { breite: number; hoehe: number }          // Pixel, Annahme oder Messung
export interface HochkantZiel { breite: number; hoehe: number }            // Pixel, Standard 1080×1920

export interface HochkantStand {
  revision: number;                 // 0 = nichts gespeichert
  layout: HochkantLayout | null;
  gespeichertAm: string | null;
  aktiveRevision: number | null;    // eingefroren in der laufenden Session
}

export interface UplinkHochkantEditorProps {
  quelle: HochkantQuelle | null;    // null = Annahme 1920×1080, sichtbar als Hinweis
  ziel: HochkantZiel;
  standbildUrl: string | null;
  standbildHinweis?: string | null; // z. B. "Vorschau aus Twitch, nicht aus Uplink"
  stand: HochkantStand | null;
  beschaeftigt: boolean;
  fehlerText?: string | null;
  onSpeichern: (layout: HochkantLayout) => void;
  onZuruecksetzen: () => void;
}
```

Reine Funktionen in `components/uplink/hochkantLayout.ts` (getestet ohne DOM): `hochkantAnfang(quelle, ziel)`, `hochkantPruefen(layout, ziel, quelle) → Fehlerliste` (Regeln 1 bis 3 plus Pixelüberstand nach `zielPixel`), `seitenverhaeltnisSperren(rahmen, verhaeltnis, quelle)`, `rahmenBegrenzen(rahmen)`, `rahmenZiehen(rahmen, griff, dx, dy, grenzen)`, `zielPixel(layout, quelle, ziel) → { gameplay, kamera, kameraBand, kameraBox }` in geraden Pixeln (die Rechnung, die der Dienst nachvollzieht), `vorschauAusschnitt(rahmen, quelle, zielflaeche)` für die Zielvorschau (liefert Pixelwerte relativ zur gemessenen Vorschaufläche).

## Offene Aufruferanforderungen

- Uplink-Service: Migration, Routen, Umrechnung beim Sessionstart (`media.rs` setzt heute `layout: None`), `active_revision` je Ziel im Status, Standbildpfad im Vorschaubudget, `vstack`-Reihenfolge für `lage = 'oben'`.
- Bot: Durchreichrouten mit Session-Identität, Twitch-Vorschaubild als Rückfall.
- Dashboard: Komponente in die Uplink-Seite einhängen (eigene Karte "Hochkant" unter den Zielen), `hochkant_enabled` je Zielkarte als Schalter.
- Entscheidung des Nutzers: Layoutwechsel während eines laufenden Streams (zusammen mit F5).
