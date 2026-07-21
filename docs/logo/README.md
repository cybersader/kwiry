# Kwiry logo

## Accepted responsive system

Kwiry uses one rounded-pixel search silhouette with two optical treatments:

| Asset | Role | Default use |
|---|---|---|
| [`kwiry.svg`](kwiry.svg) | Untouched compact mark | 64px and below |
| [`kwiry-graphite.svg`](kwiry-graphite.svg) | Balanced merged-full graphite K | 96px and above |

The size rule is intentionally simple rather than technically enforced inside one SVG. Consumers select the appropriate asset for the rendered context. Between 65px and 95px, default to the untouched mark unless the surrounding composition clearly benefits from the monogram.

### Compact mark

![Kwiry untouched compact mark](kwiry.svg)

The compact asset preserves the accepted 8×8 source geometry without an internal monogram. It is the default for app icons, tray and menu-bar icons, favicons, result rows, compact navigation, and other UI surfaces where the silhouette must remain immediate.

### Large-format graphite K

![Kwiry large-format graphite K mark](kwiry-graphite.svg)

The large asset adds the accepted balanced **merged-full graphite K**:

- one full-width three-cell merged stem;
- one full-cell center joint;
- full-cell top-right and bottom-right arm blocks;
- balanced optical spacing created by moving the stem and blocks apart without shrinking them;
- graphite faces that remain subordinate to the orange magnifier perimeter.

Use it for README headers, documentation and website heroes, setup screens, splash screens, and other branded surfaces rendered at approximately 96px or larger.

Both SVGs:

- use a transparent background;
- contain accessible `<title>` and `<desc>` elements;
- contain no fonts, scripts, raster images, or external assets;
- use the same 512×512 implementation grid derived from the accepted 8×8 construction.

A darker or one-color derivative may be explored later if a concrete platform requires it. It is not part of the accepted two-asset system yet.

## Geometry

The source silhouette occupies an exact 8×8 grid:

```text
  1 2 3 4 5 6 7 8
1 . . O O O . . .
2 . O D D D O . .
3 O D D D D D O .
4 O D D D D D O .
5 O D D D D D O .
6 . O D D D O . .
7 . . O O O . O .
8 . . . . . . . O
```

`O` is orange perimeter/handle geometry and `D` is the dark search field. The graphite K is an overlay used only by the large-format asset; it does not change the base silhouette.

### Palette

- `#FA7826` — orange highlight
- `#F66B0B` — orange face
- `#DF5902` — orange lower edge
- `#2A3140` — primary slate field
- `#1D232D` — dark transition studs
- `#3B4554` → `#151B24` — graphite K shell
- `#465162` → `#28313E` — graphite K face

## Design archive and provenance

[`kwiry-owner-reference.svg`](kwiry-owner-reference.svg) preserves the owner-supplied visual source used to recover the exact 8×8 geometry. It is a provenance reference, not a shipping logo.

Open [`kwiry-preview.html`](kwiry-preview.html) to see:

- the accepted responsive system at its intended sizes;
- source/reconstruction alignment controls;
- the exact all-orange studies that led to the final K geometry;
- spacing, gray, graphite, mixed-color, merged, pixel, and centered experiments;
- light/dark contexts and 32/64/120/256-pixel comparisons.

The earlier loose orange-K canonical placeholder and a separate image-generated concept sheet were rejected before this system was accepted. The old placeholder geometry is no longer used by either shipping asset.

## Preserved historical alternatives

These remain for project history and are not accepted branding.

### Minimal directions

- [`minimal/rounded-search-badge.svg`](minimal/rounded-search-badge.svg) — previous rounded badge
- [`minimal/pure-glass.svg`](minimal/pure-glass.svg) — reduced magnifying-glass outline
- [`minimal/k-lens-monogram.svg`](minimal/k-lens-monogram.svg) — K/lens monogram exploration
- [`minimal/escaping-focus.svg`](minimal/escaping-focus.svg) — open-focus search gesture

### Earlier rounded-pixel directions

- [`rounded-pixel-search.svg`](rounded-pixel-search.svg) — deep-ink and aqua pixel search construction
- [`pixel-note-lens.svg`](pixel-note-lens.svg) — lens framing compact note lines
- [`rounded-pixel-lens.svg`](rounded-pixel-lens.svg) — cyan-on-ink rounded pixel badge
