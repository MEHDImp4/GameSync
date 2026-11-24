import { createRoot } from "react-dom/client";
import { useEffect, useRef, useState, type FormEvent } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { SquaresFour, GameController, Waveform, Devices, SlidersHorizontal, X, ArrowLeft, ArrowRight, FolderOpen, Plus } from "@phosphor-icons/react";
import { labelsByLocale, type Locale } from "./locales";
import "./style.css";

type Transfer = { direction: "upload" | "download"; path: string; transferred: number; total: number; bytes_per_second: number };
type TransferEvent = { game: string; title: string; progress: Transfer | null };
type ServerStatus = {
  used_bytes: number; objects: number; synchronized_games: number; keep_count: number; keep_days: number;
  latest: { revision: number; game: string; title: string | null; machine: string; created_at: number }[];
  errors: { time: number; operation: string; status: number | null }[];
};
type Device = { id: string; name: string; administrator: boolean; revoked: boolean; last_seen: number | null };
type DeviceList = { current: string; administrator: boolean; items: Device[] };
type Installation = { store: string; store_id: string; title: string; directory: string; executable: string | null; proton_prefix: string | null; candidates: string[] };
type Root = { id: string; label: string };
type RemoteGame = { id: string; title: string; roots: Root[] };
type Game = { id: string; root: string; title: string; root_label: string; tracking: boolean; directory: string; executable: string; additional_roots: (Root & { directory: string })[] };
type Config = { server: { url: string; device: string } | null; games: Game[]; automatic: boolean; interval_seconds: number; bandwidth_kib: number; notifications: boolean; theme: "dark" | "light"; locale: Locale };
type Revision = { id: number; machine: string; created_at: number; published: boolean; resolved_by: number | null; bytes: number };
type History = { head: number | null; items: Revision[] };
type Status = SyncStatus["status"] | "error";
type SyncStatus =
  | { status: "unchanged" | "deferred" }
  | { status: "synchronized"; revision: number }
  | { status: "conflict"; variant: number };

type ActivityEvent = { id: number | null; message: string; time: number; game: string | null; status: Status | null };

function mergeActivity(current: ActivityEvent[], incoming: ActivityEvent[]) {
  const ids = new Set<number>();
  return [...current, ...incoming]
    .filter(({ id }) => {
      if (id === null) return true;
      if (ids.has(id)) return false;
      ids.add(id);
      return true;
    })
    .sort((a, b) => a.id !== null && b.id !== null ? b.id - a.id : b.time - a.time)
    .slice(0, 200);
}

type NavIconName = "dashboard" | "library" | "activity" | "devices" | "settings";
const navIcons = { dashboard: SquaresFour, library: GameController, activity: Waveform, devices: Devices, settings: SlidersHorizontal };
function NavIcon({ name }: { name: NavIconName }) {
  const Icon = navIcons[name];
  return <Icon className="nav-icon" weight="duotone" aria-hidden="true" />;
}
function App() {
  const [config, setConfig] = useState<Config>();
  const [tab, setTab] = useState("dashboard");
  const [editing, setEditing] = useState(false);
  const [selected, setSelected] = useState<string>();
  const [transfers, setTransfers] = useState<Record<string, TransferEvent>>({});
  const [activity, setActivity] = useState<ActivityEvent[]>([]);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [devices, setDevices] = useState<DeviceList>();
  const [serverStatus, setServerStatus] = useState<ServerStatus>();
  const [connection, setConnection] = useState<"unknown" | "online" | "offline">("unknown");
  const [invitation, setInvitation] = useState("");
  const [query, setQuery] = useState("");
  const [libraryFilter, setLibraryFilter] = useState("all");
  const [startup, setStartup] = useState<boolean>();
  const [detected, setDetected] = useState<Installation[]>([]);
  const [suggestion, setSuggestion] = useState<Installation>();
  const [catalog, setCatalog] = useState("");
  const [discovered, setDiscovered] = useState(false);
  const [remoteGames, setRemoteGames] = useState<RemoteGame[]>([]);
  const [linked, setLinked] = useState<RemoteGame>();
  const [rootCount, setRootCount] = useState(1);
  const [adding, setAdding] = useState(false);
  const [wizardStep, setWizardStep] = useState(0);
  const [advancedLink, setAdvancedLink] = useState(false);
  const wizard = useRef<HTMLDialogElement>(null);
  const wizardForm = useRef<HTMLFormElement>(null);
  const [history, setHistory] = useState<History>();
  const [moreHistory, setMoreHistory] = useState(false);
  const game = config?.games.find(({ id }) => id === selected);

  const locale = config?.locale ?? "fr";
  const labels = labelsByLocale[locale];
  function bytes(value: number) {
    const units = locale === "fr" ? ["o", "Kio", "Mio"] : ["B", "KiB", "MiB"];
    const unit = value >= 1024 ** 2 ? 2 : value >= 1024 ? 1 : 0;
    return `${(value / 1024 ** unit).toLocaleString(locale, { maximumFractionDigits: 1 })} ${units[unit]}`;
  }

  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);

  useEffect(() => {
    document.documentElement.dataset.theme = config?.theme ?? "dark";
  }, [config?.theme]);

  function refresh() {
    return invoke<Config>("configuration").then(setConfig);
  }

  useEffect(() => {
    refresh().catch((reason) => setError(String(reason)));
    invoke<Installation[]>("discover", { catalog: null }).then((items) => {
      setDetected(items);
      setDiscovered(true);
    }).catch((reason) => setError(String(reason)));
    const subscription = listen<ActivityEvent>("gamesync-activity", ({ payload }) => {
      setActivity((items) => mergeActivity([payload], items));
    });
    void subscription.then(() => invoke<ActivityEvent[]>("recent_activity"))
      .then((events) => setActivity((current) => mergeActivity(current, events)))
      .catch((reason) => setError(String(reason)));
    const progressSubscription = listen<TransferEvent>("gamesync-transfer", ({ payload }) => {
      setTransfers((current) => {
        const updates = { ...current };
        if (payload.progress) updates[payload.game] = payload;
        else delete updates[payload.game];
        return updates;
      });
    });
    for (const listener of [subscription, progressSubscription]) listener.catch((reason) => setError(String(reason)));
    return () => {
      for (const listener of [subscription, progressSubscription]) void listener.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  async function perform(action: () => Promise<string | void>) {
    setBusy(true);
    setError("");
    setMessage("");
    try {
      const result = await action();
      if (result) setMessage(result);
    } catch (reason) {
      const text = String(reason);
      setError(text);
    } finally {
      setBusy(false);
    }
  }

  async function loadDevices() {
    try {
      setDevices(await invoke<DeviceList>("devices"));
      setConnection("online");
    } catch (reason) {
      setDevices(undefined);
      setConnection("offline");
      throw reason;
    }
  }

  async function loadStatus() {
    try {
      setServerStatus(await invoke<ServerStatus>("server_status"));
      setConnection("online");
    } catch (reason) {
      setServerStatus(undefined);
      setConnection("offline");
      throw reason;
    }
  }

  useEffect(() => {
    setInvitation("");
    if (!config?.server) {
      setDevices(undefined);
      setServerStatus(undefined);
      setConnection("unknown");
      return;
    }
    if (tab === "devices" || tab === "settings") void loadDevices().catch((reason) => setError(String(reason)));
    if (tab === "dashboard" || tab === "settings") void loadStatus().catch((reason) => setError(String(reason)));
  }, [tab, config?.server?.device]);

  useEffect(() => {
    setRemoteGames([]);
    if (tab === "library" && config?.server) {
      invoke<RemoteGame[]>("server_games").then(setRemoteGames).catch((reason) => setError(String(reason)));
    }
  }, [tab, config?.server?.device]);

  async function pair(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    await perform(async () => {
      await invoke("pair", Object.fromEntries(form));
      await refresh();
      return labels.pairedNotice;
    });
  }

  useEffect(() => {
    if (adding && wizard.current && !wizard.current.open) wizard.current.showModal();
  }, [adding]);

  function openWizard(item?: Installation, remote?: RemoteGame) {
    setError(""); setMessage(""); setSuggestion(item); setLinked(remote);
    setRootCount(item ? Math.max(1, Math.min(item.candidates.length, 16)) : 1);
    setWizardStep(0); setAdvancedLink(false); setAdding(true);
  }

  function validateWizardStep() {
    const current = wizardForm.current?.querySelector<HTMLElement>(`[data-wizard-step="${wizardStep}"]`);
    for (const input of current?.querySelectorAll<HTMLInputElement>("input[required]") ?? []) {
      if (!input.reportValidity()) return false;
    }
    return true;
  }

  function nextWizardStep() {
    if (validateWizardStep()) setWizardStep((step) => Math.min(step + 1, 2));
  }

  async function choosePath(name: string, directory: boolean) {
    try {
      const selected = await open({ directory, multiple: false });
      const field = wizardForm.current?.elements.namedItem(name);
      if (selected && field instanceof HTMLInputElement) { field.value = selected; field.dispatchEvent(new Event("input", { bubbles: true })); }
    } catch (reason) { setError(String(reason)); }
  }
  async function add(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    await perform(async () => {
      const legacyGame = String(form.get("gameId") ?? "").trim() || null;
      const legacyRoot = String(form.get("rootId") ?? "").trim() || null;
      const roots = linked ? linked.roots.map((root, index) => ({
        ...root, directory: form.get(`directory-${index}`),
      })) : Array.from({ length: rootCount }, (_, index) => ({
        id: index === 0 ? legacyRoot : null,
        label: form.get(`label-${index}`),
        directory: form.get(`directory-${index}`),
      }));
      await invoke("add_game", {
        title: linked?.title ?? form.get("title"),
        executable: form.get("executable"),
        gameId: linked?.id ?? legacyGame,
        roots,
      });
      await refresh();
      setAdding(false);
      return labels.gameAdded;
    });
  }

  useEffect(() => {
    if (tab === "settings") {
      invoke<boolean>("autostart", { enabled: null }).then(setStartup).catch((reason) => setError(String(reason)));
    }
  }, [tab]);

  async function retention(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    await perform(async () => {
      await invoke("set_retention", {
        keepCount: Number(form.get("keepCount")),
        keepDays: Number(form.get("keepDays")),
      });
      await loadStatus();
      return labels.retentionSaved;
    });
  }

  async function automation(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    await perform(async () => {
      await invoke("automation", {
        automatic: form.get("automatic") === "on",
        intervalSeconds: Number(form.get("interval")),
        bandwidthKib: Number(form.get("bandwidth")),
        notifications: form.get("notifications") === "on",
      });
      await refresh();
      return labels.preferencesSaved;
    });
  }

  async function loadHistory(id: string, before?: number) {
    const page = await invoke<History>("history", { id, before: before ?? null });
    setHistory((current) => before === undefined || !current
      ? page
      : { head: page.head, items: [...current.items, ...page.items] });
    setMoreHistory(page.items.length === 50);
  }

  function select(id: string) {
    setSelected(id);
    setEditing(false);
    setHistory(undefined);
    setMoreHistory(false);
    if (config?.server) void perform(() => loadHistory(id));
  }

  function sync(id: string, revision?: number, launch = false) {
    void perform(async () => {
      const result = launch ? await invoke<SyncStatus>("synchronize_and_launch", { id }) : await invoke<SyncStatus>("synchronize", {
        id, revision: revision ?? null, expectedHead: revision === undefined ? null : history?.head,
      });
      await loadHistory(id);
      switch (result.status) {
        case "unchanged": return launch ? labels.launchedCurrent : labels.savesCurrent;
        case "deferred": return labels.syncDeferred;
        case "conflict": return labels.conflictNotice(result.variant);
        case "synchronized": return launch ? labels.launchedRevision(result.revision) : labels.synchronizedRevision(result.revision);
      }
    });
  }

  async function updateGame(event: FormEvent<HTMLFormElement>, selectedGame: Game) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const roots = [{ id: selectedGame.root, label: selectedGame.root_label }, ...selectedGame.additional_roots];
    await perform(async () => {
      await invoke("update_game", {
        id: selectedGame.id, executable: form.get("executable"),
        roots: roots.map(({ id, label }) => ({ id, label, directory: form.get(id) })),
      });
      await refresh();
      setEditing(false);
      return labels.pathsSaved;
    });
  }

  const recentErrors = activity.filter((event) => event.status === "error").slice(0, 4);
  const lastSync = activity.find((item) => item.status === "synchronized")?.time;
  function stateFor(item: Game) {
    if (!item.tracking) return "paused";
    const status = activity.find((event) => event.game === item.id)?.status;
    if (status === "conflict" || status === "error" || status === "deferred") return status;
    return status ? "current" : "new";
  }
  const stateLabels = { paused: labels.paused, conflict: labels.conflict, error: labels.error, deferred: labels.deferred, current: labels.current, new: labels.needsSync };
  const needsAttention = (item: Game) => ["conflict", "error", "deferred"].includes(stateFor(item));
  const pending = config?.games.filter((item) => item.tracking && (needsAttention(item) || stateFor(item) === "new")).length ?? 0;
  const trackedGames = config?.games.filter((item) => item.tracking).length ?? 0;
  const games = config?.games.filter((item) => item.title.toLocaleLowerCase(locale).includes(query.trim().toLocaleLowerCase(locale))
    && (libraryFilter === "all" || (libraryFilter === "attention" ? needsAttention(item) : !item.tracking))) ?? [];
  return (
    <Tabs.Root value={tab} onValueChange={setTab} className="shell">
      <aside className="sidebar">
        <div className="brand"><span className="brand-mark" aria-hidden="true"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M20 7v5h-5M4 17v-5h5"/><path d="M5.6 8a7 7 0 0 1 11.6-2L20 12M4 12l2.8 6a7 7 0 0 0 11.6-2"/></svg></span><span>GameSync<small>DESKTOP</small></span></div>
        <Tabs.List className="navigation" aria-label={labels.navigation}>
          <Tabs.Trigger value="dashboard"><NavIcon name="dashboard" /><span>{labels.dashboard}</span></Tabs.Trigger>
          <Tabs.Trigger value="library"><NavIcon name="library" /><span>{labels.library}</span></Tabs.Trigger>
          <Tabs.Trigger value="activity"><NavIcon name="activity" /><span>{labels.activity}</span></Tabs.Trigger>
          <Tabs.Trigger value="devices"><NavIcon name="devices" /><span>{labels.devices}</span></Tabs.Trigger>
          <Tabs.Trigger value="settings"><NavIcon name="settings" /><span>{labels.settings}</span></Tabs.Trigger>
        </Tabs.List>
        <div className="device-state"><span className={config?.server ? "paired" : ""} />{config?.server ? labels.paired : labels.pairingRequired}</div>
      </aside>
      <main className="workspace">
        {error && <p className="notice error" role="alert">{error}</p>}
        {message && <p className="notice" role="status">{message}</p>}
        <Tabs.Content value="dashboard">
          <header className="page-header"><div><h1>{labels.dashboard}</h1><p>{labels.saveStatus}</p></div>{config?.server && <button disabled={busy} onClick={() => void perform(loadStatus)}>{labels.refresh}</button>}</header>
          <div className="summary-grid">
            <section className="panel"><h2>{labels.trackedGames}</h2><strong>{trackedGames}</strong></section>
            <section className="panel"><h2>{labels.server}</h2><strong>{!config?.server ? labels.pairingRequired : connection === "online" ? labels.online : connection === "offline" ? labels.offline : labels.checking}</strong></section>
            <section className="panel"><h2>{labels.deferred}</h2><strong>{pending}</strong></section>
            <section className="panel"><h2>{labels.lastSync}</h2><strong>{lastSync ? new Date(lastSync).toLocaleString(locale) : labels.neverSynced}</strong></section>
          </div>
          {!config?.server && <button className="primary" onClick={() => setTab("settings")}>{labels.pairDevice}</button>}
          {recentErrors.length > 0 && <section className="panel"><h2>{labels.clientErrors}</h2>
            {recentErrors.map((event, index) => <div className="revision" key={event.id ?? `error-${index}`}><span>{event.message}</span><time>{new Date(event.time).toLocaleString(locale)}</time></div>)}
          </section>}
          {activity.length > 0 && <section className="panel"><h2>{labels.recentActivity}</h2>
            {activity.slice(0, 4).map((event, index) => <div className="revision" key={event.id ?? `event-${index}`}><span>{event.message}</span><time>{new Date(event.time).toLocaleTimeString(locale)}</time></div>)}
          </section>}
          {serverStatus && <section className="panel">
            <h2>{labels.serverStorage}</h2>
            <p>{bytes(serverStatus.used_bytes)} · {labels.storageSummary(serverStatus.objects, serverStatus.synchronized_games)}</p>
            <p className="muted">{labels.retentionSummary(serverStatus.keep_count, serverStatus.keep_days)}</p>
            {serverStatus.latest.length > 0 && <><h3>{labels.latestPublications}</h3>
              {serverStatus.latest.map((item) => <div className="revision" key={item.revision}>
                <span>{item.title ?? item.game} · {labels.revision(item.revision)}</span><time>{new Date(item.created_at * 1000).toLocaleString(locale)}</time>
              </div>)}
            </>}
            {serverStatus.errors.length > 0 && <><h3>{labels.serverErrors}</h3>
              {serverStatus.errors.map((item, index) => <div className="revision" key={index}>
                <span>{item.operation}{item.status ? ` · HTTP ${item.status}` : ""}</span><time>{new Date(item.time * 1000).toLocaleString(locale)}</time>
              </div>)}
            </>}
          </section>}
        </Tabs.Content>
        <Tabs.Content value="library">
          <header className="page-header">
            <div><h1>{labels.library}</h1><p>{labels.gamesSummary(config?.games.length ?? 0)}</p></div>
            <button className="primary" onClick={() => openWizard()} disabled={busy}><Plus size={16} weight="bold" aria-hidden="true" />{labels.addGame}</button>
          </header>
          {adding && <dialog ref={wizard} className="wizard-dialog" aria-labelledby="wizard-title" onClose={() => setAdding(false)} onCancel={() => setAdding(false)}>
            <form ref={wizardForm} key={linked?.id ?? (suggestion ? `${suggestion.store}:${suggestion.store_id}` : "manual")} className="wizard" noValidate onSubmit={(event) => { if (wizardStep !== 2) { event.preventDefault(); nextWizardStep(); } else if (!validateWizardStep()) { event.preventDefault(); } else void add(event); }}>
              <header className="wizard-header">
                <div><p className="wizard-eyebrow">{labels.wizardStep(wizardStep + 1)}</p><h2 id="wizard-title">{linked ? labels.linkGame(linked.title) : labels.addGame}</h2></div>
                <button type="button" className="icon-button" aria-label={labels.cancel} onClick={() => setAdding(false)}><X size={20} aria-hidden="true" /></button>
              </header>
              <ol className="wizard-progress" aria-label={labels.wizardProgress}>
                {[labels.wizardGame, labels.wizardSaves, labels.wizardExecutable].map((label, index) => <li key={label} aria-current={wizardStep === index ? "step" : undefined} className={index < wizardStep ? "completed" : ""}><span>{index + 1}</span>{label}</li>)}
              </ol>
              {error && <p className="notice error" role="alert">{error}</p>}
              <div className="wizard-body" data-wizard-step="0" hidden={wizardStep !== 0}>
                <div><h3>{labels.wizardGameTitle}</h3><p className="muted">{labels.wizardGameHelp}</p></div>
                {linked ? <p className="wizard-selected">{linked.title}</p> : <label>{labels.gameName}<input name="title" defaultValue={suggestion?.title} required /></label>}
                {suggestion && <p className="path-hint">{labels.detectedInstallation(suggestion.directory)}</p>}
                {!linked && rootCount === 1 && <div className="advanced-link">
                  <button type="button" className="text-button" onClick={() => setAdvancedLink(!advancedLink)}>{labels.attachById}</button>
                  {advancedLink && <div className="wizard-fields"><label>{labels.gameId}<input name="gameId" /></label><label>{labels.rootId}<input name="rootId" /></label></div>}
                </div>}
              </div>
              <div className="wizard-body" data-wizard-step="1" hidden={wizardStep !== 1}>
                <div><h3>{labels.wizardSavesTitle}</h3><p className="muted">{labels.wizardSavesHelp}</p></div>
                {(linked?.roots ?? Array.from({ length: rootCount }, (_, index) => ({ id: String(index), label: index === 0 ? labels.saves : labels.folderNumber(index + 1) }))).map((root, index) => <fieldset className="root-form" key={root.id}>
                  {!linked && rootCount > 1 && <label>{labels.folderLabel}<input name={`label-${index}`} defaultValue={root.label} required /></label>}
                  {!linked && rootCount === 1 && <input type="hidden" name="label-0" value={root.label} />}
                  <label>{linked ? root.label : labels.saveFolder}<span className="path-input"><input name={`directory-${index}`} list="save-candidates" defaultValue={linked ? "" : suggestion?.candidates[index] ?? ""} required /><button type="button" onClick={() => void choosePath(`directory-${index}`, true)} aria-label={labels.browseFolder}><FolderOpen size={18} aria-hidden="true" />{labels.browse}</button></span></label>
                </fieldset>)}
                <datalist id="save-candidates">{suggestion?.candidates.map((path) => <option value={path} key={path} />)}</datalist>
                {!linked && <div className="wizard-folder-actions"><button type="button" disabled={busy || rootCount >= 16} onClick={() => setRootCount(rootCount + 1)}><Plus size={16} aria-hidden="true" />{labels.addFolder}</button>{rootCount > 1 && <button type="button" disabled={busy} onClick={() => setRootCount(rootCount - 1)}>{labels.removeFolder}</button>}</div>}
              </div>
              <div className="wizard-body" data-wizard-step="2" hidden={wizardStep !== 2}>
                <div><h3>{labels.wizardExecutableTitle}</h3><p className="muted">{labels.wizardExecutableHelp}</p></div>
                <label>{labels.executablePath}<span className="path-input"><input name="executable" defaultValue={suggestion?.executable ?? ""} required /><button type="button" onClick={() => void choosePath("executable", false)} aria-label={labels.browseFile}><FolderOpen size={18} aria-hidden="true" />{labels.browse}</button></span></label>
              </div>
              <footer className="wizard-footer">
                {wizardStep > 0 ? <button type="button" onClick={() => setWizardStep(wizardStep - 1)}><ArrowLeft size={16} aria-hidden="true" />{labels.previous}</button> : <button type="button" onClick={() => setAdding(false)}>{labels.cancel}</button>}
                {wizardStep < 2 ? <button type="button" className="primary" onClick={nextWizardStep}>{labels.next}<ArrowRight size={16} aria-hidden="true" /></button> : <button className="primary" disabled={busy}>{labels.saveGame}</button>}
              </footer>
            </form>
          </dialog>}
          <section className="panel">
            <div className="library-filters">
              <label>{labels.searchGames}<input type="search" value={query} onChange={(event) => setQuery(event.target.value)} /></label>
              <label>{labels.status}<select value={libraryFilter} onChange={(event) => setLibraryFilter(event.target.value)}>
                <option value="all">{labels.all}</option><option value="attention">{labels.needsAttention}</option><option value="paused">{labels.paused}</option>
              </select></label>
            </div>
            {games.map((item) => <button className="game-row" key={item.id} disabled={busy} aria-pressed={selected === item.id} onClick={() => select(item.id)}>
              <span className="game-icon" aria-hidden="true">{item.title.slice(0, 2).toLocaleUpperCase(locale)}</span>
              <span>{item.title}<small className="muted"> · {stateLabels[stateFor(item)]}</small></span><span aria-hidden="true">›</span>
            </button>)}
            {games.length === 0 && <p className="muted">{query || libraryFilter !== "all" ? labels.noMatchingGames : labels.addFirstGame}</p>}
          </section>
          <section className="panel">
            <div className="section-header"><h2>{labels.installedGames}</h2>
              <button disabled={busy} onClick={() => void perform(async () => {
                setDetected(await invoke<Installation[]>("discover", { catalog: catalog.trim() || null }));
                setDiscovered(true);
              })}>{labels.scanLibraries}</button>
            </div>
            <p className="muted">{labels.folderScanHelp}</p>
            <button disabled={busy} onClick={() => void perform(async () => {
              const directory = await open({ directory: true, multiple: false });
              if (!directory) return;
              const found = await invoke<Installation[]>("discover_folder", { directory, catalog: catalog.trim() || null });
              setDetected((current) => {
                const merged = new Map(current.map((item) => [item.directory.toLocaleLowerCase(), item]));
                for (const item of found) {
                  const key = item.directory.toLocaleLowerCase();
                  const previous = merged.get(key);
                  merged.set(key, previous ? { ...previous, executable: previous.executable ?? item.executable, candidates: [...new Set([...previous.candidates, ...item.candidates])] } : item);
                }
                return [...merged.values()].sort((a, b) => a.title.localeCompare(b.title));
              });
              setDiscovered(true);
              return found.length ? labels.folderResults(found.length) : labels.noFolderGames;
            })}>{labels.scanFolder}</button>

            {detected.filter((item) => item.title.toLocaleLowerCase(locale).includes(query.trim().toLocaleLowerCase(locale))).map((item) =>
              <div className="revision" key={`${item.store}:${item.store_id}`}>
                <div><strong>{item.title}</strong><span>{item.store}{item.proton_prefix ? " · Proton" : ""}</span>
                  <p>{item.candidates.length ? labels.locationsToConfirm(item.candidates.length) : labels.saveFolderRequired}</p>{item.candidates.map((path) => <p className="muted" key={path}>{path}</p>)}
                </div>
                <button disabled={busy} onClick={() => openWizard(item)}>{labels.configure}</button>
              </div>
            )}
            {discovered && detected.length === 0 && <p className="muted">{labels.noInstallations}</p>}
          </section>
          {config?.server && <section className="panel">
            <div className="section-header"><h2>{labels.serverGames}</h2>
              <button disabled={busy} onClick={() => void perform(async () => {
                setRemoteGames(await invoke<RemoteGame[]>("server_games"));
              })}>{labels.refresh}</button>
            </div>
            {remoteGames.filter((remote) => !config.games.some((local) => local.id === remote.id)).map((remote) =>
              <div className="revision" key={remote.id}>
                <strong>{remote.title}</strong>
                <button disabled={busy} onClick={() => openWizard(undefined, remote)}>{labels.attachDevice}</button>
              </div>
            )}
            {!remoteGames.some((remote) => !config.games.some((local) => local.id === remote.id)) && <p className="muted">{labels.noRemoteGames}</p>}
          </section>}
          {game && <section className="panel">
            <div className="section-header"><h2>{game.title}</h2><div className="game-actions">
              <button disabled={busy} onClick={() => setEditing(!editing)}>{editing ? labels.closeEditor : labels.editPaths}</button>
              <button disabled={busy} onClick={() => void perform(async () => {
                await invoke("set_tracking", { id: game.id, tracking: !game.tracking });
                await refresh();
                return game.tracking ? labels.trackingPaused : labels.trackingResumed;
              })}>{game.tracking ? labels.pauseTracking : labels.resumeTracking}</button>
              <button disabled={busy || !game.tracking || !config?.server} onClick={() => sync(game.id)}>{labels.sync}</button>
              <button className="primary" disabled={busy || !game.tracking || !config?.server} onClick={() => sync(game.id, undefined, true)}>{labels.syncLaunch}</button>
            </div></div>
            {editing && <form className="root-form" onSubmit={(event) => void updateGame(event, game)}>
              {[{ id: game.root, label: game.root_label, directory: game.directory }, ...game.additional_roots].map((root) =>
                <label key={root.id}>{root.label}<input name={root.id} defaultValue={root.directory} required /></label>)}
              <label>{labels.executable}<input name="executable" defaultValue={game.executable} required /></label>
              <button className="primary" disabled={busy}>{labels.savePaths}</button>
            </form>}
            <dl className="details">
              <dt>{game.root_label}</dt><dd>{game.directory}</dd>
              {game.additional_roots.map((root) => <div className="extra-root" key={root.id}><dt>{root.label}</dt><dd>{root.directory}</dd></div>)}
              <dt>{labels.executable}</dt><dd>{game.executable}</dd>
              <dt>{labels.gameId}</dt><dd>{game.id}</dd>
              <dt>{labels.rootId}</dt><dd>{game.root}</dd>
            </dl>
            <div className="section-header"><h3>{labels.history}</h3>
              <button disabled={busy || !config?.server} onClick={() => void perform(() => loadHistory(game.id))}>{labels.refreshHistory}</button>
            </div>
            {history?.items.length ? history.items.map((item) => <div className="revision" key={item.id}>
              <div><strong>{labels.revision(item.id)}</strong><span>{item.id === history.head ? labels.currentRevision : item.published ? labels.history : item.resolved_by ? labels.resolvedBy(item.resolved_by) : labels.conflictVariant}</span>
                <p>{new Date(item.created_at * 1000).toLocaleString(locale)} · {bytes(item.bytes)} · {item.machine.slice(0, 8)}</p>
              </div>
              <button disabled={busy || !game.tracking || history.head === null} onClick={() => {
                if (window.confirm(labels.confirmApply(item.id))) sync(game.id, item.id);
              }}>{labels.apply}</button>
            </div>) : <p className="muted">{config?.server ? labels.noRevisions : labels.pairForHistory}</p>}
            {moreHistory && history && <button disabled={busy} onClick={() => void perform(() => loadHistory(game.id, history.items.at(-1)?.id))}>{labels.olderVersions}</button>}
          </section>}
        </Tabs.Content>
        <Tabs.Content value="activity">
          <header className="page-header"><div><h1>{labels.activity}</h1><p>{labels.activityDescription}</p></div></header>
          {Object.values(transfers).map(({ game: id, title, progress }) => progress && <section className="panel transfer" key={id}>
            <div className="section-header"><h2>{title}</h2><span>{progress.direction === "upload" ? labels.upload : labels.download}</span></div>
            <p title={progress.path}>{progress.path}</p>
            <progress aria-label={labels.transferFile(progress.path)} max={progress.total || 1} value={progress.transferred || (progress.total === 0 ? 1 : 0)} />
            <div className="section-header"><span>{bytes(progress.transferred)} / {bytes(progress.total)}</span><span>{bytes(progress.bytes_per_second)}/s</span></div>
          </section>)}
          <section className="panel">{activity.length ? activity.map((item, index) => <div className="revision" key={item.id ?? `event-${index}`}>
            <span>{item.message}</span><time>{new Date(item.time).toLocaleTimeString(locale)}</time>
          </div>) : <p className="muted">{labels.noRecentEvents}</p>}</section>
        </Tabs.Content>
        <Tabs.Content value="devices">
          <header className="page-header"><div><h1>{labels.devices}</h1><p>{labels.registeredDevices}</p></div>
            <button disabled={busy || !config?.server} onClick={() => void perform(loadDevices)}>{labels.refresh}</button>
          </header>
          {devices?.administrator && <section className="panel form">
            <button className="primary" disabled={busy} onClick={() => void perform(async () => {
              setInvitation(await invoke<string>("invite_device"));
            })}>{labels.createInvite}</button>
            {invitation && <label>{labels.inviteValidity}
              <input readOnly value={invitation} onFocus={(event) => event.currentTarget.select()} />
            </label>}
          </section>}
          <section className="panel">
            {devices?.items.map((device) => <div className="revision" key={device.id}>
              <div><strong>{device.name}</strong><span>{device.id === devices.current ? labels.thisDevice : device.administrator ? labels.administrator : labels.device}</span>
                <p>{device.revoked ? labels.revoked : device.last_seen ? labels.lastActivity(new Date(device.last_seen * 1000).toLocaleString(locale)) : labels.unknownActivity}</p>
              </div>
              {devices.administrator && device.id !== devices.current && !device.revoked && <button disabled={busy} onClick={() => {
                if (window.confirm(labels.confirmRevoke(device.name))) void perform(async () => {
                  await invoke("revoke_device", { id: device.id });
                  await loadDevices();
                  return labels.deviceRevoked;
                });
              }}>{labels.revoke}</button>}
            </div>)}
            {!devices && <p className="muted">{config?.server ? labels.devicesUnavailable : labels.pairForDevices}</p>}
          </section>
        </Tabs.Content>
        <Tabs.Content value="settings">
          <header className="page-header"><div><h1>{labels.settings}</h1><p>{labels.serverConnection}</p></div></header>
          {config?.server && <section className="panel"><h2>{labels.registeredServer}</h2><p>{config.server.url}</p><p className="muted">{labels.device} {config.server.device}</p></section>}
          {config && <form className="panel form" onSubmit={automation}>
            <h2>{labels.synchronization}</h2>
            <label className="check"><input name="automatic" type="checkbox" defaultChecked={config.automatic} />{labels.enableBackground}</label>
            <label>{labels.intervalSeconds}<input name="interval" type="number" min="30" max="3600" defaultValue={config.interval_seconds} required /></label>
            <label className="check"><input name="notifications" type="checkbox" defaultChecked={config.notifications} />{labels.notifyProblems}</label>
            <label>{labels.bandwidth}<input name="bandwidth" type="number" min="0" max="1048576" defaultValue={config.bandwidth_kib} required /></label>
            <button className="primary" disabled={busy}>{labels.save}</button>
          </form>}
          {devices?.administrator && serverStatus && <form
            key={`${devices.current}:${serverStatus.keep_count}:${serverStatus.keep_days}`}
            className="panel form"
            onSubmit={retention}
          >
            <h2>{labels.serverRetention}</h2>
            <p>{labels.retentionExplanation}</p>
            <label>{labels.minVersions}<input name="keepCount" type="number" min="1" max="4294967295" defaultValue={serverStatus.keep_count} required /></label>
            <label>{labels.minDays}<input name="keepDays" type="number" min="1" max="4294967295" defaultValue={serverStatus.keep_days} required /></label>
            <button className="primary" disabled={busy}>{labels.saveRetention}</button>
          </form>}
          <form className="panel form" onSubmit={pair}>
            <h2>{labels.pairing}</h2>
            <label>{labels.serverAddress}<input name="address" type="url" defaultValue={config?.server?.url ?? ""} placeholder="https://gamesync.example" required /></label>
            <label>{labels.deviceName}<input name="name" required /></label>
            <label>{labels.pairingCode}<input name="code" type="password" autoComplete="off" required /></label>
            <button className="primary" disabled={busy}>{labels.pairDevice}</button>
          </form>
          {config && <section className="panel form">
            <h2>{labels.appearance}</h2>
            <label>{labels.language}<select name="locale" value={locale} disabled={busy} onChange={(event) => {
              const next = event.currentTarget.value;
              void perform(async () => { await invoke("set_locale", { locale: next }); await refresh(); });
            }}>
              <option value="fr" lang="fr">Français</option>
              <option value="en" lang="en">English</option>
            </select></label>
            <label>{labels.theme}<select name="theme" value={config.theme} disabled={busy} onChange={(event) => {
              const theme = event.currentTarget.value;
              void perform(async () => { await invoke("set_theme", { theme }); await refresh(); });
            }}>
              <option value="dark">{labels.dark}</option>
              <option value="light">{labels.light}</option>
            </select></label>
          </section>}
          <section className="panel">
            <h2>{labels.startup}</h2>
            <label className="check"><input type="checkbox" checked={startup ?? false} disabled={busy || startup === undefined} onChange={(event) => {
              const enabled = event.currentTarget.checked;
              void perform(async () => {
                setStartup(await invoke<boolean>("autostart", { enabled }));
                return enabled ? labels.startupEnabled : labels.startupDisabled;
              });
            }} />{labels.startOnLogin}</label>
          </section>
          <section className="panel form">
            <h2>{labels.catalogSettings}</h2>
            <p className="muted">{labels.catalogHelp}</p>
            <label>{labels.manifestPath}<input value={catalog} onChange={(event) => setCatalog(event.target.value)} placeholder={labels.catalogAutomatic} /></label>
          </section>
          <button disabled={busy} onClick={() => void perform(() => invoke("quit"))}>{labels.quit}</button>
        </Tabs.Content>
      </main>
    </Tabs.Root>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
