// SPDX-License-Identifier: GPL-3.0
//
// Builds FlyByWire's A380X OANS, with the harness in ./src, into two packages, each as one
// script and one stylesheet with the fonts and images they use, and writes each one's
// layout.json:
//   packages/msfs-a320-oans          the OANS on the Fenix A320's captain ND (instrument.tsx)
//   packages/msfs-amdb-oans-toolbar  the Airport Map toolbar window (toolbar.tsx)
//
// FlyByWire's source is fetched at a pinned commit into ./.fbw (sparse, not committed).
// Each FlyByWire file keeps resolving its imports through its own tsconfig, as in their
// build; only the harness uses ./tsconfig.json.
//
//   cd tools/fenix-oans && npm install && node build.mjs

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import * as sass from 'sass';

const FBW_REPO = 'https://github.com/flybywiresim/aircraft.git';
const FBW_COMMIT = '2baa2b35eadaf4c78e172ce41bbe6b40b4aeafb2';
const FBW_DIRS = [
  'fbw-common/src/systems',
  'fbw-common/src/typings',
  'fbw-common/src/localization',
  'fbw-a32nx/src/systems/fmgc',
  'fbw-a32nx/src/systems/shared',
  'fbw-a380x/src/systems/instruments/src/ND',
  'fbw-a380x/src/systems/instruments/src/MFD',
  'fbw-a380x/src/systems/instruments/src/MsfsAvionicsCommon',
  'fbw-a380x/src/systems/shared',
];

const here = path.dirname(fileURLToPath(import.meta.url));
const fbw = path.join(here, '.fbw');

/**
 * What is built. Each package has its own asset folder (MSFS merges every package's
 * html_ui into one file system, so two packages must not ship the same path) and reports
 * its load and its errors to the bridge under its own name.
 */
const TARGETS = [
  {
    pkg: path.resolve(here, '../../packages/msfs-a320-oans'),
    out: 'html_ui/Pages/VCockpit/Instruments/amdb-oans',
    entry: 'src/instrument.tsx',
    name: 'oans-nd',
    assetDir: 'amdb-a320-oans',
    tag: 'amdb-oans',
  },
  {
    pkg: path.resolve(here, '../../packages/msfs-amdb-oans-toolbar'),
    out: 'html_ui/InGamePanels/AmdbOansMap',
    entry: 'src/toolbar.tsx',
    name: 'AmdbOansMap',
    assetDir: 'amdb-oans-toolbar',
    tag: 'AmdbOansMap',
  },
];

// What the display draws with comes from the package's own assets, not FlyByWire's:
// their licence covers their code (GPL) and 3D models, but not their fonts or images. The
// font is B612, the typeface Airbus commissioned for cockpit displays (SIL Open Font
// License, assets/fonts/B612-OFL.txt); the flag and cross are drawn for these packages.
function assets(dir) {
  return {
    [`Fonts/${dir}/B612Mono-Regular.ttf`]: 'assets/fonts/B612Mono-Regular.ttf',
    [`Images/${dir}/oans/oans-cross.svg`]: 'assets/images/oans-cross.svg',
    [`Images/${dir}/oans/oans-flag.svg`]: 'assets/images/oans-flag.svg',
  };
}

function relinks(dir) {
  const font = `/Fonts/${dir}/B612Mono-Regular.ttf`;
  return [
    ['/Fonts/fbw-a380x/FBW-Display-EIS-A380-SlashedZero.ttf', font],
    ['/Fonts/fbw-a380x/FBW-Display-EIS-A380.ttf', font],
    ['/Fonts/fbw-a380x/NDChrono.ttf', font],
    ['/Images/fbw-a380x/oans/oans-cross.png', `/Images/${dir}/oans/oans-cross.svg`],
    ['/Images/fbw-a380x/oans/oans-flag.png', `/Images/${dir}/oans/oans-flag.svg`],
    // The erase dialog, which adds "cross.svg" or "flag.svg" itself.
    ['/Images/fbw-a380x/oans/oans-', `/Images/${dir}/oans/oans-`],
  ];
}

function git(...args) {
  return execFileSync('git', args, { cwd: fbw, stdio: ['ignore', 'pipe', 'inherit'] }).toString().trim();
}

function fetchFbw() {
  if (fs.existsSync(path.join(fbw, '.git')) && git('rev-parse', 'HEAD') === FBW_COMMIT) {
    git('sparse-checkout', 'set', ...FBW_DIRS);
    return;
  }
  fs.rmSync(fbw, { recursive: true, force: true });
  fs.mkdirSync(fbw);
  git('init', '-q');
  git('config', 'core.longpaths', 'true');
  git('remote', 'add', 'origin', FBW_REPO);
  git('sparse-checkout', 'init', '--cone');
  git('sparse-checkout', 'set', ...FBW_DIRS);
  git('fetch', '-q', '--depth', '1', '--filter=blob:none', 'origin', FBW_COMMIT);
  git('checkout', '-q', 'FETCH_HEAD');
}

function copyAssets({ pkg, assetDir }) {
  for (const dir of ['Fonts', 'Images']) {
    fs.rmSync(path.join(pkg, 'html_ui', dir), { recursive: true, force: true });
  }
  for (const [to, from] of Object.entries(assets(assetDir))) {
    fs.mkdirSync(path.dirname(path.join(pkg, 'html_ui', to)), { recursive: true });
    fs.copyFileSync(path.join(here, from), path.join(pkg, 'html_ui', to));
  }
  fs.copyFileSync(path.join(here, 'assets/fonts/B612-OFL.txt'), path.join(pkg, 'LICENSE-B612-font.txt'));
}

/** Point the bundle's font and image paths at the package's own assets, and check each exists. */
function relinkAssets(file, { pkg, assetDir }) {
  let text = fs.readFileSync(file, 'utf8');
  for (const [from, to] of relinks(assetDir)) {
    text = text.split(from).join(to);
  }
  const left = text.match(/\/(Fonts|Images)\/fbw-a380x\/[\w./-]*/);
  if (left) {
    throw new Error(`${path.basename(file)} still uses FlyByWire's ${left[0]}: add it to relinks()`);
  }
  for (const [ref, dir] of text.matchAll(/\/((?:Fonts|Images)\/[\w-]+\/[\w./-]*)/g)) {
    // A path cut short where the script adds the rest (oans-flag / oans-cross) is checked
    // as the folder it points into.
    const local = path.join(pkg, 'html_ui', dir);
    const exists = /\.\w+$/.test(dir) ? fs.existsSync(local) : fs.existsSync(path.dirname(local));
    if (!exists) {
      throw new Error(`${path.basename(file)} uses ${ref}, which the package does not have: add it to assets()`);
    }
  }
  fs.writeFileSync(file, text);
}

/** MSFS's list of a package's files, and the size it states in its manifest. */
function writeLayout({ pkg }) {
  const manifestPath = path.join(pkg, 'manifest.json');
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  const writeManifest = () => fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}
`);
  // Written once before measuring, so its own size is counted as it will be; the total is
  // a fixed 20 digits, so stamping it in afterwards does not change that size.
  writeManifest();
  const files = [];
  const walk = (dir) => {
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, e.name);
      if (e.isDirectory()) {
        walk(full);
      } else if (path.relative(pkg, full) !== 'layout.json') {
        files.push(full);
      }
    }
  };
  walk(pkg);
  const content = files
    .map((full) => {
      const st = fs.statSync(full, { bigint: true });
      // MSFS records the paths lower-cased, and the times as Windows FILETIME ticks, which
      // are too large for a JavaScript number to hold exactly.
      const ticks = st.mtimeNs / 100n + 116444736000000000n;
      return { path: path.relative(pkg, full).split(path.sep).join('/').toLowerCase(), size: Number(st.size), date: `${ticks}` };
    })
    .sort((a, b) => (a.path < b.path ? -1 : 1));
  manifest.total_package_size = String(content.reduce((n, f) => n + f.size, 0)).padStart(20, '0');
  writeManifest();
  const layout = JSON.stringify({ content }, null, 2).replace(/"date": "(\d+)"/g, '"date": $1');
  fs.writeFileSync(path.join(pkg, 'layout.json'), `${layout}
`);
}

const scss = {
  name: 'scss',
  setup(b) {
    b.onLoad({ filter: /\.scss$/ }, (args) => ({
      contents: sass.compile(args.path, { loadPaths: [path.dirname(args.path)] }).css,
      loader: 'css',
    }));
  },
};

// FlyByWire asks Navigraph's API for the airport through a signed-in Navigraph session;
// here the same queries go to the local bridge, so that one module is swapped.
const bridgeAmdb = {
  name: 'bridge-amdb',
  setup(b) {
    b.onLoad({ filter: /[\\/]shared[\\/]src[\\/]navigraph[\\/]amdb\.ts$/ }, () => ({
      contents: fs.readFileSync(path.join(here, 'src/amdb-bridge.ts'), 'utf8'),
      loader: 'ts',
    }));
  },
};

// Small changes to FlyByWire's own files, applied as they are bundled so the checkout
// stays as fetched. Each must match exactly once, or the build stops: a FlyByWire change
// that moves the code is caught here instead of silently dropping the change.
const FBW_PATCHES = {
  // Building names run along the building's long axis instead of flat across the map.
  'OANC/Oanc.tsx': [
    [
      '            position: labelPosition,\n            rotation: undefined,\n            associatedFeature: feature,\n',
      '            position: labelPosition,\n            rotation: feature.properties.feattype === FeatureType.VerticalPolygonalStructure ? fenixBuildingAxisBearing(feature) : undefined,\n            associatedFeature: feature,\n',
    ],
    [
      /$/,
      `
/** Bearing from north of a building's long axis: the direction most of its outline runs. */
function fenixBuildingAxisBearing(feature: Feature<Geometry>): number | undefined {
  const g = feature.geometry as Polygon | MultiPolygon;
  const ring = g.type === 'Polygon' ? g.coordinates[0] : g.type === 'MultiPolygon' ? g.coordinates[0]?.[0] : undefined;
  if (!ring || ring.length < 3) {
    return undefined;
  }
  const weight = new Array<number>(180).fill(0);
  for (let i = 1; i < ring.length; i++) {
    const dx = ring[i][0] - ring[i - 1][0];
    const dy = ring[i][1] - ring[i - 1][1];
    const length = Math.hypot(dx, dy);
    const bearing = Math.round(((Math.atan2(dx, dy) * 180) / Math.PI + 360) % 180);
    for (let d = -10; d <= 10; d++) {
      weight[(bearing + d + 180) % 180] += length;
    }
  }
  return weight.indexOf(Math.max(...weight));
}
`,
    ],
  ],
  // Once a runway is picked for BTV, only the exits its landings can take (the bridge tags
  // each exit with them as idthr); data without the tag shows every exit, as before.
  // BTV is armed from the MAP DATA page, in the place of LDG SHIFT, which FlyByWire keeps
  // disabled; the state comes from FenixBtv on the bus and lives in L:AMDB_BTV_ARM.
  'ND/OansControlPanel.tsx': [
    [
      '                      <Button\n' +
        '                        label="LDG SHIFT"\n' +
        '                        onClick={() => this.showLdgShiftPanel()}\n' +
        '                        buttonStyle="flex: 1"\n' +
        '                        disabled={Subject.create(true)}\n' +
        '                      />\n',
      '                      <Button\n' +
        "                        label={ConsumerSubject.create(this.props.bus.getSubscriber<any>().on('amdb_btv_armed'), false).map((a) => (a ? 'DISARM BTV' : 'ARM BTV'))}\n" +
        "                        onClick={() => SimVar.SetSimVarValue('L:AMDB_BTV_ARM', 'number', SimVar.GetSimVarValue('L:AMDB_BTV_ARM', 'number') > 0.5 ? 0 : 1)}\n" +
        '                        buttonStyle="flex: 1"\n' +
        "                        disabled={ConsumerSubject.create(this.props.bus.getSubscriber<any>().on('amdb_btv_can_arm'), false).map((c) => !c)}\n" +
        '                      />\n',
    ],
  ],
  'OANC/OansBrakeToVacateSelection.ts': [
    // The layer is cleared under whatever transform the last drawing left, which is moved
    // to the canvas centre: the part above and left of the centre was never cleared, so a
    // taxi route taken away stayed there in part. It is cleared untransformed.
    [
      "    this.canvasRef.instance\n      .getContext('2d')\n      ?.clearRect(0, 0, this.canvasRef.instance.width, this.canvasRef.instance.height);\n",
      "    this.canvasRef.instance.getContext('2d')?.resetTransform();\n" +
        "    this.canvasRef.instance\n      .getContext('2d')\n      ?.clearRect(0, 0, this.canvasRef.instance.width, this.canvasRef.instance.height);\n",
    ],
    // The picked exit's type (the bridge marks high-speed exits), for BTV's release speed.
    [
      "    this.bus.getPublisher<FmsOansData>().pub('oansSelectedExit', exit, true);\n",
      "    this.bus.getPublisher<FmsOansData>().pub('oansSelectedExit', exit, true);\n" +
        "    this.bus.getPublisher<any>().pub('amdb_btv_exit_type', feature.properties?.exittype ?? 1, true);\n",
    ],
    // The taxi route the bridge keeps (typed in the OANS toolbar window), drawn in magenta
    // on the BTV layer, which moves and turns with the map.
    [
      '  private btvPathGeometry: Position[] = [];\n',
      '  private btvPathGeometry: Position[] = [];\n\n' +
        '  /** The taxi route the bridge keeps, in the map\'s own coordinates. */\n' +
        '  private amdbTaxiRoute: Position[] = [];\n',
    ],
    [
      '    this.zoomLevelIndex?.sub(() => this.drawBtvLayer());\n',
      '    this.zoomLevelIndex?.sub(() => this.drawBtvLayer());\n' +
        "    (this.sub as any).on('amdb_taxi_route').handle((route: Position[] | null) => {\n" +
        '      this.amdbTaxiRoute = route ?? [];\n' +
        '      this.drawBtvLayer();\n' +
        '    });\n',
    ],
    ['    this.drawBtvPath();\n', '    this.drawAmdbTaxiRoute();\n    this.drawBtvPath();\n'],
    [
      '  drawBtvLayer() {\n',
      '  drawAmdbTaxiRoute() {\n' +
        "    const ctx = this.canvasRef?.instance.getContext('2d');\n" +
        '    if (this.amdbTaxiRoute.length < 2 || !this.canvasRef?.getOrDefault() || !ctx) {\n' +
        '      return;\n' +
        '    }\n' +
        '    ctx.resetTransform();\n' +
        '    ctx.translate(this.canvasCentreX?.get() ?? 0, this.canvasCentreY?.get() ?? 0);\n' +
        '    ctx.lineWidth = 5;\n' +
        "    ctx.lineJoin = 'round';\n" +
        "    ctx.lineCap = 'round';\n" +
        "    ctx.strokeStyle = '#ff94ff';\n" +
        '    const path = new Path2D();\n' +
        '    path.moveTo(this.amdbTaxiRoute[0][0], this.amdbTaxiRoute[0][1] * -1);\n' +
        '    for (let i = 1; i < this.amdbTaxiRoute.length; i++) {\n' +
        '      path.lineTo(this.amdbTaxiRoute[i][0], this.amdbTaxiRoute[i][1] * -1);\n' +
        '    }\n' +
        '    ctx.stroke(path);\n' +
        '  }\n\n' +
        '  drawBtvLayer() {\n',
    ],
  ],
  'OANC/OancLabelFilter.ts': [
    [
      '  switch (filter.type) {\n',
      '  const exitServes =\n' +
        '    label.associatedFeature?.properties.feattype === FeatureType.RunwayExitLine ? label.associatedFeature.properties.idthr : undefined;\n' +
        "  if (btvSelectedRunway && typeof exitServes === 'string' && !exitServes.split('.').includes(btvSelectedRunway.substring(4))) {\n" +
        '    return false;\n' +
        '  }\n' +
        '  switch (filter.type) {\n',
    ],
  ],
  'OANC/OancLabelManager.ts': [
    [
      "          } else {\n            element.style.transform = 'translate(-50%, -50%)';\n          }\n",
      "          } else if (label.style === LabelStyle.TerminalBuilding && label.rotation !== undefined) {\n" +
        '            // Along the building, never upside down.\n' +
        '            let angle = ((((label.rotation - mapCurrentHeading - 90) % 360) + 540) % 360) - 180;\n' +
        '            angle = angle > 90 ? angle - 180 : angle <= -90 ? angle + 180 : angle;\n' +
        '            element.style.transform = `translate(-50%, -50%) rotate(${angle}deg)`;\n' +
        "          } else {\n            element.style.transform = 'translate(-50%, -50%)';\n          }\n",
    ],
  ],
};

// Fenix's display renders its texture much brighter than the A380's, which suits its own
// black ND with thin lines but blows FlyByWire's mid-grey airport surfaces out to white.
// Surface fills are darkened for it; the thin lines and the labels are left as they are.
const FENIX_DIM = { surface: 0.6, runwayPaint: 0.85, building: 0.75 };

function dim(css, factor) {
  const hex = css === 'gray' ? '#808080' : css;
  const m = /^#([0-9a-f]{6})$/i.exec(hex);
  if (!m) {
    throw new Error(`style-data.ts: cannot darken colour ${css}`);
  }
  const n = parseInt(m[1], 16);
  return `#${[n >> 16, (n >> 8) & 255, n & 255].map((c) => Math.round(c * factor).toString(16).padStart(2, '0')).join('')}`;
}

function dimStyleData(text) {
  let fills = 0;
  const out = text
    .replace(/fillStyle: '([^']+)'/g, (_, c) => {
      fills++;
      const factor = c === '#ffffff' ? FENIX_DIM.runwayPaint : c === '#00ffff' || c === '#3286da' ? FENIX_DIM.building : FENIX_DIM.surface;
      return `fillStyle: '${dim(c, factor)}'`;
    })
    // The zoomed-out taxiways are drawn as wide grey strokes rather than fills.
    .replace(/strokeStyle: '#666666'/g, () => `strokeStyle: '${dim('#666666', FENIX_DIM.surface)}'`);
  if (fills === 0) {
    throw new Error('style-data.ts: no fill colours found to darken');
  }
  return out;
}

/** The FlyByWire files the patches below were applied to, checked after the build. */
const patched = new Set();

const fbwPatches = {
  name: 'fbw-patches',
  setup(b) {
    b.onLoad({ filter: /[\\/](OANC[\\/](Oanc\.tsx|OancLabelManager\.ts|OancLabelFilter\.ts|OansBrakeToVacateSelection\.ts|style-data\.ts)|fbw-a380x[\\/].*[\\/]ND[\\/]OansControlPanel\.tsx)$/ }, (args) => {
      const key = `${path.basename(path.dirname(args.path))}/${path.basename(args.path)}`;
      patched.add(key);
      // Git on Windows may check the files out with CRLF line endings.
      let text = fs.readFileSync(args.path, 'utf8').replace(/\r\n/g, '\n');
      if (key === 'OANC/style-data.ts') {
        return { contents: dimStyleData(text), loader: 'ts' };
      }
      for (const [from, to] of FBW_PATCHES[key]) {
        if (typeof from === 'string') {
          const n = text.split(from).length - 1;
          if (n !== 1) {
            throw new Error(`${key}: expected the patched code once, found it ${n} times`);
          }
        }
        text = text.replace(from, to);
      }
      return { contents: text, loader: args.path.endsWith('.tsx') ? 'tsx' : 'ts' };
    });
  },
};

async function buildTarget(target) {
  const { pkg, out, entry, name, tag } = target;
  const outDir = path.join(pkg, out);
  patched.clear();
  copyAssets(target);
  await build({
    absWorkingDir: here,
    entryPoints: { [name]: path.join(here, entry) },
    outdir: outDir,
    bundle: true,
    format: 'iife',
    // Approximately CoherentGT's WebKit, as FlyByWire targets it.
    target: 'safari11',
    jsxFactory: 'FSComponent.buildComponent',
    jsxFragment: 'FSComponent.Fragment',
    // FlyByWire's build fills these from its .env; the simulator has no `process`, so any
    // left in the bundle throws the moment it loads (checked after the build).
    define: {
      DEBUG: 'false',
      'process.env.NODE_ENV': '"production"',
      'process.env.NODE_DEBUG': '""',
      'process.env.CLIENT_ID': '""',
      'process.env.CLIENT_SECRET': '""',
      'process.env.AIRCRAFT_PROJECT_PREFIX': '"a380x"',
      'process.env.AIRCRAFT_VARIANT': '"A380-842"',
    },
    // Errors in a panel gauge are otherwise invisible: report ours, and a successful load,
    // to the bridge, which logs any request it does not recognise.
    banner: {
      js: [
        `window.addEventListener('error',function(e){if(String(e.filename).indexOf('${tag}')>=0){fetch('http://127.0.0.1:8770/${tag}-error?'+encodeURIComponent(e.message+' @'+e.lineno+':'+e.colno));}});`,
        `window.addEventListener('unhandledrejection',function(e){var r=e.reason,s=r&&r.stack?String(r.stack):'';if(s.indexOf('${tag}')>=0){fetch('http://127.0.0.1:8770/${tag}-error?'+encodeURIComponent(String(r&&r.message||r)+' | '+s.slice(0,300)));}});`,
      ].join('\n'),
    },
    footer: { js: `fetch('http://127.0.0.1:8770/${tag}-loaded');` },
    plugins: [scss, bridgeAmdb, fbwPatches],
    // Absolute paths in FlyByWire's stylesheets are MSFS's own virtual file system, resolved
    // by the simulator at run time (see relinkAssets).
    external: ['/Fonts/*', '/Images/*'],
    logLevel: 'warning',
    legalComments: 'eof',
  });
  for (const f of [`${name}.js`, `${name}.css`]) {
    relinkAssets(path.join(outDir, f), target);
  }
  // A file the filter above never matched would otherwise go unpatched without a word.
  const unpatched = [...Object.keys(FBW_PATCHES), 'OANC/style-data.ts'].filter((k) => !patched.has(k));
  if (unpatched.length) {
    console.error(`never patched (the file was not bundled, or the filter missed it): ${unpatched.join(', ')}`);
    process.exit(1);
  }
  const leftover = [...new Set(fs.readFileSync(path.join(outDir, `${name}.js`), 'utf8').match(/process\.env\.\w+/g) ?? [])];
  if (leftover.length) {
    console.error(`the bundle still reads ${leftover.join(', ')}: add them to \`define\` in build.mjs`);
    process.exit(1);
  }
  writeLayout(target);
  console.log(`built ${path.relative(process.cwd(), pkg)} from FlyByWire ${FBW_COMMIT.slice(0, 8)}`);
}

fetchFbw();
for (const target of TARGETS) {
  await buildTarget(target);
}
