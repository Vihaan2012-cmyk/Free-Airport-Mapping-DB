Airport Map (OANS toolbar window)
=================================

An airport moving map in a window of its own, for any aircraft: FlyByWire's A380X
OANS, as the A320 OANS shows it on the Fenix A320's ND. Open it from the toolbar
(the runway icon, "Airport Map").

The bar under the map:
  ARC / ROSE / PLAN   how the map is drawn: ahead of the aircraft, around it, or
                      north up
  - 0.5 NM +          the range: 0.2, 0.5, 1, 2 or 5 NM
  MAP DATA            the OANS control panel: pick the airport, runways, status
  TAXI                a taxi clearance: the taxiways in order, then a runway or a
                      stand, as the controller gives it:
                        A B K 31L
                        B K STAND 73    (or: B K 73)
                        22R             (no taxiways: the shortest way there)
                      Enter (or ROUTE) finds the way from where the aircraft is,
                      along those taxiways, and draws it in magenta. CLEAR takes
                      it away. The A320 OANS on the Fenix ND draws the same route.

Click the map for the context menu (flags, crosses, map data), drag to pan.
Resize the window as you like: the map fills the largest square it can.

Airport maps and routes come from a bridge running on this computer while you
fly: AMDB Bridge, or the "A320 OANS" program. This package does not include one;
its installer asks where yours is. Download either from
https://github.com/Vihaan2012-cmyk/Free-Airport-Mapping-DB

Routes follow the airport's taxiway network in the map data. Where that data
has no taxiway by a name you give, the window says so rather than guessing.

Credits and licence
-------------------
The display is FlyByWire Simulations' A380X OANS, used as they wrote it:
  https://github.com/flybywiresim/aircraft
  (commit 2baa2b35eadaf4c78e172ce41bbe6b40b4aeafb2, 2026-09-20)
Copyright (c) FlyByWire Simulations and its contributors.

None of FlyByWire's fonts or images are included. The display font is B612,
the typeface Airbus commissioned for cockpit displays, Copyright 2012 The B612
Project Authors, under the SIL Open Font License 1.1 (LICENSE-B612-font.txt).
The flag, cross and toolbar symbols were drawn for this package.

Because it contains FlyByWire's code, this package is licensed under the GNU
General Public License, version 3 (LICENSE.txt), unlike the rest of AMDB
Bridge, which is MIT. The source for everything in it, and the script that
builds it from FlyByWire's source at the commit above, is in tools/fenix-oans
in the AMDB Bridge repository.

Not affiliated with or endorsed by FlyByWire Simulations.
NOT FOR REAL-WORLD NAVIGATION.
