// Proof-of-life for the Fenix A320 OANS work. Nothing here talks to Fenix's own
// systems or WASM module -- it only proves our own htmlgauge content really runs
// inside Fenix's panel, by publishing an L:Var an external SimConnect client can
// read back. The real OANS toggle/range gauge replaces this once injection itself
// is confirmed working live.
//
// Structured after Fenix's own LOD gauge, which is the same invisible NO_TEXTURE
// manager pattern: a BaseInstrument subclass whose templateID matches the template
// in the .html beside this file, driven by the sim's per-frame Update() rather than
// requestAnimationFrame (a gauge rendering to no texture gets no frame callbacks).
class AmdbOansShell extends BaseInstrument {
    constructor() {
        super();
        this.ticks = 0;
    }

    get templateID() {
        return "AmdbOansShell";
    }

    Update() {
        super.Update();
        this.ticks++;
        SimVar.SetSimVarValue("L:AMDB_OANS_SHELL_LOADED", "number", 1);
        SimVar.SetSimVarValue("L:AMDB_OANS_SHELL_TICKS", "number", this.ticks);
    }
}

registerInstrument("amdb-oans-shell-element", AmdbOansShell);
