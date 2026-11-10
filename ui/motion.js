(function (root) {
  "use strict";

  const reducedQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
  const contrastQuery = window.matchMedia("(prefers-contrast: more)");
  const state = { visible: true, ball: false, preset: "lively", theme: null, resting: false };
  const stats = { rafCallbacks: 0, draws: 0, particles: 0 };
  let engine = null;
  let restTimer = null;

  function bake(color, dpr, halo, vivid) {
    const unit = 10;
    const box = 48;
    const canvas = document.createElement("canvas");
    canvas.width = box * dpr;
    canvas.height = box * dpr;
    const context = canvas.getContext("2d");
    context.scale(dpr, dpr);
    if (halo) {
      context.globalAlpha = vivid ? 0.42 : 0.26;
      context.fillStyle = color;
      context.beginPath(); context.arc(box / 2, box / 2, unit * 2.1, 0, Math.PI * 2); context.fill();
    }
    context.globalAlpha = 1;
    context.fillStyle = color;
    context.beginPath(); context.arc(box / 2, box / 2, unit, 0, Math.PI * 2); context.fill();
    if (vivid) {
      context.globalAlpha = 0.72;
      context.fillStyle = "#FFFFFF";
      context.beginPath(); context.arc(box / 2, box / 2, unit * 0.32, 0, Math.PI * 2); context.fill();
    }
    return { canvas, box, unit };
  }

  function spawn(anywhere) {
    const angle = Math.random() * Math.PI * 2;
    const radius = Math.sqrt(Math.random()) * 23;
    return {
      x: 30 + Math.cos(angle) * radius,
      y: anywhere ? 30 + Math.sin(angle) * radius : 58,
      speed: 5 + Math.random() * 9,
      sway: 1 + Math.random() * 2.5,
      phase: Math.random() * Math.PI * 2,
      size: 0.8 + Math.random() * 1.2,
      color: Math.floor(Math.random() * engine.sprites.length),
    };
  }

  function draw(time, dt) {
    const context = engine.context;
    context.clearRect(0, 0, 60, 60);
    context.save();
    context.beginPath(); context.arc(30, 30, 29, 0, Math.PI * 2); context.clip();
    context.globalCompositeOperation = "lighter";
    for (const mote of engine.motes) {
      mote.y -= mote.speed * dt;
      if (mote.y < -4) Object.assign(mote, spawn(false));
      const sprite = engine.sprites[mote.color];
      const x = mote.x + Math.sin(time * 0.8 + mote.phase) * mote.sway;
      const size = sprite.box * mote.size / sprite.unit;
      const pulse = 0.5 + 0.5 * Math.sin(time * 1.8 + mote.phase);
      context.globalAlpha = engine.stardust ? 0.48 + 0.52 * pulse : 0.25 + 0.55 * pulse;
      context.drawImage(sprite.canvas, x - size / 2, mote.y - size / 2, size, size);
    }
    engine.orbiters.forEach((orbiter, index) => {
      orbiter.angle += orbiter.speed * dt;
      for (let trail = 3; trail >= 0; trail -= 1) {
        const angle = orbiter.angle - orbiter.speed * trail * 0.07;
        const x = 30 + Math.cos(angle) * orbiter.radius;
        const y = 30 + Math.sin(angle) * orbiter.radius;
        const sprite = trail === 0 ? engine.head : engine.sprites[index % engine.sprites.length];
        const size = sprite.box * (trail === 0 ? 1.25 : 0.9) / sprite.unit;
        context.globalAlpha = trail === 0 ? 0.95 : 0.18;
        context.drawImage(sprite.canvas, x - size / 2, y - size / 2, size, size);
      }
    });
    context.restore();
    context.globalAlpha = 1;
    context.globalCompositeOperation = "source-over";
    stats.draws += 1;
  }

  function tick(timestamp) {
    if (!engine) return;
    stats.rafCallbacks += 1;
    const dt = Math.min((timestamp - engine.last) / 1000 || 1 / 30, 0.06);
    engine.last = timestamp;
    draw(timestamp / 1000, dt);
    engine.timer = setTimeout(() => {
      if (engine) engine.raf = requestAnimationFrame(tick);
    }, 1000 / 30);
  }

  function allowed() {
    const animated = state.preset === "stardust" || state.preset === "lively" && !state.resting;
    return state.visible && state.ball && animated && !document.hidden && !reducedQuery.matches && !contrastQuery.matches;
  }

  function start() {
    if (engine || !allowed()) return;
    const canvas = document.querySelector(".orb-fx");
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.round(60 * dpr);
    canvas.height = Math.round(60 * dpr);
    const context = canvas.getContext("2d");
    context.setTransform(dpr, 0, 0, dpr, 0, 0);
    const palette = state.theme ? state.theme.palette : { particles: ["#60DFC8", "#A78BFA"], gradient: ["#55E6D0", "#52C4F4", "#748FFF", "#B18BF4", "#EC9CD7"], text: "#EEF3FF" };
    const stardust = state.preset === "stardust";
    const colors = stardust ? [...new Set([...palette.particles, ...(palette.gradient || [])])] : [...palette.particles, palette.text];
    const orbiters = [{ angle: 0, radius: 18, speed: 0.55 }];
    if (stardust) orbiters.push({ angle: Math.PI, radius: 26, speed: -0.42 });
    const particleCount = stardust ? 16 : 8;
    engine = {
      context, dpr, last: performance.now(), timer: null, raf: null, stardust,
      sprites: colors.map((color) => bake(color, dpr, true, stardust)),
      head: bake(palette.text, dpr, false),
      motes: [],
      orbiters,
    };
    engine.motes = Array.from({ length: particleCount }, () => spawn(true));
    stats.particles = particleCount;
    engine.raf = requestAnimationFrame(tick);
  }

  function stop() {
    if (!engine) return;
    clearTimeout(engine.timer);
    cancelAnimationFrame(engine.raf);
    engine.context.clearRect(0, 0, 60, 60);
    engine = null;
    stats.particles = 0;
  }

  function syncRest() {
    clearTimeout(restTimer);
    state.resting = false;
    document.body.classList.remove("fx-resting");
    if (state.preset === "lively" && state.ball && state.visible && !document.hidden && !reducedQuery.matches && !contrastQuery.matches) {
      restTimer = setTimeout(() => {
        state.resting = true;
        document.body.classList.add("fx-resting");
        stop();
      }, 15000);
    }
  }

  function sync() {
    document.body.classList.toggle("motion-static", state.preset === "static");
    document.body.classList.toggle("motion-lively", state.preset === "lively");
    document.body.classList.toggle("motion-stardust", state.preset === "stardust");
    document.body.classList.toggle("high-contrast", contrastQuery.matches);
    syncRest();
    if (allowed()) start(); else stop();
  }

  function setState(next) {
    const oldDpr = engine && engine.dpr;
    const oldPreset = state.preset;
    const restart = engine && (next.theme || oldDpr !== Math.min(window.devicePixelRatio || 1, 2) || next.preset && next.preset !== oldPreset);
    if (restart) stop();
    Object.assign(state, next);
    sync();
  }

  function poke() {
    if (state.preset === "lively") sync();
  }

  document.addEventListener("visibilitychange", sync);
  window.addEventListener("resize", () => setState({}));
  [reducedQuery, contrastQuery].forEach((query) => {
    if (query.addEventListener) query.addEventListener("change", sync);
    else query.addListener(sync);
  });
  ["pointermove", "pointerdown", "keydown"].forEach((event) => window.addEventListener(event, poke, { passive: true }));

  root.KQBMotion = {
    setState,
    sync,
    poke,
    start,
    stop,
    getStats: () => ({ ...stats, running: Boolean(engine) }),
    resetStats: () => { stats.rafCallbacks = 0; stats.draws = 0; },
  };
})(window);
