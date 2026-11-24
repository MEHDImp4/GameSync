async (input) => {
  const invoke = window.__TAURI_INTERNALS__.invoke;

  async function waitFor(probe, description) {
    const end = Date.now() + 15000;
    while (Date.now() < end) {
      const value = probe();
      if (value) return value;
      const error = document.querySelector('[role="alert"]');
      if (error) throw new Error(error.textContent);
      await new Promise((resolve) => setTimeout(resolve, 60));
    }
    throw new Error(`Timed out waiting for ${description}`);
  }

  const buttons = () => Array.from(document.querySelectorAll("button"));
  const findButton = (label, scope = document) => [...scope.querySelectorAll("button")].find((button) => button.textContent.trim() === label && !button.disabled);
  const click = async (label) => {
    const button = await waitFor(() => findButton(label), label);
    button.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0 }));
    button.click();
  };
  const status = (text) => waitFor(() => document.querySelector('[role="status"]')?.textContent.includes(text), text);
  const field = (name, value) => {
    const element = document.querySelector(`input[name="${name}"]`);
    if (!element) throw new Error(`Missing field ${name}`);
    element.value = value;
  };

  try {
    if (input.code) {
      await click("Paramètres");
      await waitFor(() => document.querySelector('input[name="code"]'), "pairing form");
      field("address", input.address);
      field("name", input.device);
      field("code", input.code);
      await click("Appairer cet appareil");
      await status("Appareil appairé");
    }

    if (input.mode === "seed") {
      await waitFor(() => document.querySelector('input[name="keepCount"]'), "retention form");
      field("keepCount", "25");
      field("keepDays", "45");
      await click("Enregistrer la rétention");
      await status("Rétention du serveur enregistrée");
      await click("Tableau de bord");
      await waitFor(() => document.body.textContent.includes("25 révision(s) et 45 jours"), "retention on dashboard");
      await click("Paramètres");
      const theme = await waitFor(() => document.querySelector('select[name=theme]'), "theme selector");
      theme.value = "light";
      theme.dispatchEvent(new Event("change", { bubbles: true }));
      await waitFor(() => document.documentElement.dataset.theme === "light", "light theme");
    }
    if (input.mode === "locale") {
      await waitFor(() => document.documentElement.lang === "en" && findButton("Settings"), "persisted English locale");
      await click("Library");
      const game = await waitFor(() => buttons().find((button) => button.classList.contains("game-row") && button.textContent.includes(input.title)), "configured game");
      game.click();
      await click("Sync");
      await status("Saves are up to date");
      await click("Activity");
      await waitFor(() => document.querySelector('[data-state="active"][role="tabpanel"]')?.textContent.includes(`${input.title} · Saves are up to date`), "English activity event");
      await invoke("smoke_result", { error: null });
      return;
    }
    if (input.mode === "resolved") {
      await waitFor(() => document.documentElement.dataset.theme === "light", "restored theme");
    }
    await click("Bibliothèque");
    if (input.mode === "seed" || input.mode === "attach") {
      if (input.mode === "seed") {
        await click("Ajouter un jeu");
        await waitFor(() => document.querySelector('input[name="title"]'), "manual game form");
        field("title", input.title);
      } else {
        const row = await waitFor(
          () => [...document.querySelectorAll(".revision")].find((entry) => entry.textContent.includes(input.title) && findButton("Rattacher à cet appareil", entry)),
          "server game catalog",
        );
        findButton("Rattacher à cet appareil", row).click();
      }
      await click("Continuer");
      await waitFor(() => document.querySelector('[data-wizard-step="1"]:not([hidden]) input[name="directory-0"]'), "save directory step");
      field("directory-0", input.directory);
      await click("Continuer");
      await waitFor(() => document.querySelector('[data-wizard-step="2"]:not([hidden]) input[name="executable"]'), "executable step");
      field("executable", input.executable);
      await click("Enregistrer le jeu");
      await status("Jeu ajouté");
    }

    const game = await waitFor(
      () => buttons().find((button) => button.classList.contains("game-row") && button.textContent.includes(input.title) && !button.disabled),
      "configured game",
    );
    if (input.mode === "offline") {
      game.click();
      const alert = await waitFor(() => document.querySelector('[role="alert"]'), "offline error");
      if (!alert.textContent.includes("HTTP")) throw new Error(alert.textContent);
      await invoke("smoke_result", { error: null });
      return;
    }
    game.click();
    await click("Synchroniser");
    if (input.mode === "active") {
      await status("Synchronisation reportée");
      await invoke("smoke_result", { error: null });
      return;
    }
    if (input.mode === "conflict") {
      await status("conservée");
      const variant = await waitFor(
        () => [...document.querySelectorAll(".revision")].find((row) => row.textContent.includes("Variante en conflit") && findButton("Appliquer", row)),
        "conflict variant",
      );
      window.confirm = () => true;
      findButton("Appliquer", variant).click();
      await status("synchronisée");
      await waitFor(() => document.body.textContent.includes("Résolue par #"), "resolved variant");
    } else {
      await status("synchronisée");
    }
    await click("Activité");
    await waitFor(() => document.body.textContent.includes(input.title), "persistent activity");
    if (input.mode === "resolved") {
      await click("Paramètres");
      const language = await waitFor(() => document.querySelector('select[name="locale"]'), "language selector");
      language.value = "en";
      language.dispatchEvent(new Event("change", { bubbles: true }));
      await waitFor(() => document.documentElement.lang === "en" && findButton("Settings"), "English locale");
      await click("Dashboard");
      await waitFor(() => document.querySelector("h1")?.textContent === "Dashboard", "English dashboard");
    }
    await invoke("smoke_result", { error: null });
  } catch (error) {
    await invoke("smoke_result", { error: String(error) });
  }
}
