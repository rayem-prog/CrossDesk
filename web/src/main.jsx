import React, { useEffect, useMemo, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import {
  Activity,
  CheckCircle2,
  CircleStop,
  ExternalLink,
  Info,
  Keyboard,
  MonitorCog,
  Play,
  RefreshCw,
  Save,
  Settings,
  Sparkles,
  TerminalSquare,
  Wifi,
  X,
} from 'lucide-react';
import brandIconUrl from '../../assets/deskbridge5-transparent-flat.png';
import './style.css';

const APP_VERSION = 'v1.6.9';
const BRAND_ICON_DATA = 'data:image/webp;base64,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';
const AUTHOR = 'paulhe666';
const REPO_URL = 'https://github.com/paulhe666/deskbridge';
const RELEASES_URL = `${REPO_URL}/releases`;

const mappingOptions = [
  ['control', 'Control / Ctrl'],
  ['meta', 'Meta / Command / Win'],
  ['alt', 'Alt / Option'],
  ['shift', 'Shift'],
  ['disabled', 'Disabled'],
];

const keyTargetOptions = [
  ['escape', 'Escape'],
  ['backspace', 'Backspace'],
  ['delete', 'Delete'],
  ['enter', 'Enter / Return'],
  ['tab', 'Tab'],
  ['space', 'Space'],
  ['caps_lock', 'CapsLock'],
  ['arrow_left', 'Arrow Left'],
  ['arrow_right', 'Arrow Right'],
  ['arrow_up', 'Arrow Up'],
  ['arrow_down', 'Arrow Down'],
  ['disabled', 'Disabled'],
];

const copy = {
  en: {
    subtitle: '',
    stopped: 'Service stopped',
    running: 'Service running',
    loading: 'Loading',
    connection: 'Connection',
    runtime: 'Runtime',
    logs: 'Logs',
    mode: 'Mode',
    client: 'Client',
    server: 'Server',
    serverAddress: 'Server address',
    bindAddress: 'Bind address',
    edge: 'Remote device position',
    left: 'Remote on left',
    right: 'Remote on right',
    language: 'Language',
    commandPreview: 'Command preview',
    start: 'Start',
    stop: 'Stop',
    save: 'Save',
    saving: 'Saving',
    saved: 'Saved',
    saveFailed: 'Save failed',
    keyboardScopeTitle: 'Server-side only',
    keyboardScopeText: 'Keyboard mappings only affect the computer running Deskbridge as the server. Changing them on a client does not take effect.',
    refresh: 'Refresh',
    clear: 'Clear',
    noLogs: 'No logs yet.',
    settings: 'Settings',
    general: 'General',
    keyboard: 'Keyboard Mapping',
    about: 'About / Update',
    activeMappings: 'Active mappings',
    modifierMappings: 'Modifier keys',
    specialMappings: 'Special keys',
    updateAvailable: 'Update available',
    upToDate: 'You are up to date',
    updateUnknown: 'Update status unknown',
    version: 'Version',
    author: 'Maintainer',
    repo: 'Repository',
    checkUpdates: 'Check updates',
    manualUpdate: 'Manual update',
    autoUpdate: 'Auto update check',
    autoUpdateText: 'Automatically check GitHub Releases when Deskbridge starts. Installers are still downloaded manually for safety.',
    updateHint: 'Check GitHub Releases and open the download page for the latest installer.',
    configPath: 'Config file path',
    configPathHint: 'Leave as the shown program-folder path, or enter a custom .ini file path. Non-.ini paths are treated as folders and config.ini will be placed inside.',
    useDefaultConfigPath: 'Use program folder',
  },
  zh: {
    subtitle: '',
    stopped: '服务已停止',
    running: '服务运行中',
    loading: '加载中',
    connection: '连接',
    runtime: '运行',
    logs: '日志',
    mode: '模式',
    client: '客户端',
    server: '服务端',
    serverAddress: '服务端地址',
    bindAddress: '监听地址',
    edge: '远端设备位置',
    left: '远端在左侧',
    right: '远端在右侧',
    language: '语言',
    commandPreview: '命令预览',
    start: '启动',
    stop: '停止',
    save: '保存',
    saving: '保存中',
    saved: '已保存',
    saveFailed: '保存失败',
    keyboardScopeTitle: '仅服务端生效',
    keyboardScopeText: '键盘映射只作用于运行 Deskbridge 服务端的电脑；在客户端修改不会生效。',
    refresh: '刷新',
    clear: '清空',
    noLogs: '暂无日志。',
    settings: '设置',
    general: '通用',
    keyboard: '键盘映射',
    about: '关于 / 更新',
    activeMappings: '当前生效映射',
    modifierMappings: '修饰键',
    specialMappings: '特殊键',
    updateAvailable: '发现新版本',
    upToDate: '当前已是最新版',
    updateUnknown: '暂未检查更新',
    version: '版本号',
    author: '维护者',
    repo: '仓库',
    checkUpdates: '检查更新',
    manualUpdate: '手动更新',
    autoUpdate: '自动检查更新',
    autoUpdateText: '启动 Deskbridge 时自动检查 GitHub Releases。为了安全，安装包仍由你手动下载和安装。',
    updateHint: '检查 GitHub Releases，并打开最新版安装包下载页面。',
    configPath: '配置文件路径',
    configPathHint: '可以保持程序目录下的默认路径，也可以输入自定义 .ini 文件路径。非 .ini 路径会被当作目录，并在其中保存 config.ini。',
    useDefaultConfigPath: '使用程序目录',
  },
};

function BrandMark() {
  return <img src={brandIconUrl} alt="" aria-hidden="true" className="brand-image" />;
}

function App() {
  const [state, setState] = useState(null);
  const [config, setConfig] = useState(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [saveNotice, setSaveNotice] = useState('');
  // These refs prevent the 1.2s polling refresh from overwriting text the user
  // is actively editing before it has been saved.
  const configDirtyRef = useRef(false);
  const configEditingRef = useRef(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState('general');
  const [updateInfo, setUpdateInfo] = useState(null);
  const [updateBusy, setUpdateBusy] = useState(false);

  async function invokeBackendCommand(commandName, args = {}, options = {}) {
    const { preserveLocalConfig = false } = options;
    setError('');
    const payload = await invoke(commandName, args);
    setState(payload);
    if (payload.config && !(preserveLocalConfig && (configDirtyRef.current || configEditingRef.current))) {
      setConfig(payload.config);
    }
    return payload;
  }

  async function refresh() {
    try {
      await invokeBackendCommand('get_state', {}, { preserveLocalConfig: true });
    } catch (err) {
      setError(String(err));
    }
  }

  async function persistConfig(nextConfig = config) {
    if (!nextConfig) return false;
    setSaveNotice('saving');
    const payload = await invoke('save_config', { config: nextConfig });
    setState(payload);
    if (payload.config) {
      setConfig(payload.config);
    }
    configDirtyRef.current = false;
    configEditingRef.current = false;
    setSaveNotice('saved');
    window.setTimeout(() => setSaveNotice(''), 1800);
    return true;
  }

  async function save(nextConfig = config) {
    setBusy(true);
    setError('');
    try {
      await persistConfig(nextConfig);
      return true;
    } catch (err) {
      setError(String(err));
      setSaveNotice('error');
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function runServiceCommand(commandName) {
    setBusy(true);
    setError('');
    try {
      if (commandName === 'start_service' && config && (configDirtyRef.current || configEditingRef.current)) {
        try {
          await persistConfig(config);
        } catch (err) {
          setSaveNotice('error');
          throw err;
        }
      }
      await invokeBackendCommand(commandName, {}, { preserveLocalConfig: commandName === 'start_service' });
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  async function checkUpdates() {
    setUpdateBusy(true);
    setError('');
    try {
      const info = await invoke('check_for_updates');
      setUpdateInfo(info);
    } catch (err) {
      setUpdateInfo({ error: String(err) });
      setError(String(err));
    } finally {
      setUpdateBusy(false);
    }
  }

  async function openReleases() {
    setError('');
    try {
      await invoke('open_release_page');
    } catch (err) {
      setError(String(err));
    }
  }

  useEffect(() => {
    refresh();
    const timer = setInterval(refresh, 1200);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    if (config?.autoUpdateCheck && !updateInfo && !updateBusy) {
      checkUpdates();
    }
  }, [config?.autoUpdateCheck]);

  const commandPreview = state?.commandPreview || 'deskbridge';
  const logs = state?.logs || [];
  const running = Boolean(state?.running);
  const role = config?.role || 'client';
  const t = copy[config?.language === 'zh' ? 'zh' : 'en'];

  const statusText = useMemo(() => {
    if (!state) return t.loading;
    return running ? t.running : t.stopped;
  }, [state, running, t]);

  function patchConfig(patch) {
    configDirtyRef.current = true;
    setConfig((current) => ({ ...current, ...patch }));
  }

  function markConfigEditing(editing) {
    configEditingRef.current = editing;
  }

  function patchAndSave(patch) {
    setConfig((current) => {
      const next = { ...current, ...patch };
      save(next);
      return next;
    });
  }

  return (
    <main className="app-shell">
      <section className="hero-card">
        <div className="brand-row">
          <div className="app-mark image-mark" aria-label="Deskbridge icon">
            <BrandMark />
          </div>
          <div>
            <h1>Deskbridge</h1>
            {t.subtitle && <p>{t.subtitle}</p>}
          </div>
        </div>
        <div className="hero-actions">
          <div className={`status-pill ${running ? 'online' : 'offline'}`}>
            <Activity size={16} />
            {statusText}
          </div>
          <button className="icon-button" title={t.settings} onClick={() => setSettingsOpen(true)}>
            <Settings size={21} strokeWidth={2.2} />
          </button>
        </div>
      </section>

      {error && <div className="error-banner">{error}</div>}

      <section className="grid-layout">
        <Panel className="connection-panel" title={t.connection} icon={<Wifi size={18} />}>
          {!config ? (
            <Skeleton />
          ) : (
            <div className="form-grid">
              <label>
                {t.mode}
                <select value={role} onChange={(event) => patchAndSave({ role: event.target.value })}>
                  <option value="client">{t.client}</option>
                  <option value="server">{t.server}</option>
                </select>
              </label>

              {role === 'client' ? (
                <label className="wide">
                  {t.serverAddress}
                  <input
                    value={config.server}
                    placeholder="192.168.1.10:24920"
                    onFocus={() => markConfigEditing(true)}
                    onBlur={() => markConfigEditing(false)}
                    onChange={(event) => patchConfig({ server: event.target.value })}
                  />
                </label>
              ) : (
                <>
                  <label className="wide">
                    {t.bindAddress}
                    <input
                      value={config.bind}
                      placeholder="0.0.0.0:24920"
                      onFocus={() => markConfigEditing(true)}
                      onBlur={() => markConfigEditing(false)}
                      onChange={(event) => patchConfig({ bind: event.target.value })}
                    />
                  </label>
                  <label>
                    {t.edge}
                    <select value={config.edge} onChange={(event) => patchAndSave({ edge: event.target.value })}>
                      <option value="left">{t.left}</option>
                      <option value="right">{t.right}</option>
                    </select>
                  </label>
                </>
              )}

              <label>
                {t.language}
                <select value={config.language} onChange={(event) => patchAndSave({ language: event.target.value })}>
                  <option value="zh">中文</option>
                  <option value="en">English</option>
                </select>
              </label>
            </div>
          )}
        </Panel>

        <Panel className="runtime-panel" title={t.runtime} icon={<MonitorCog size={18} />}>
          <div className="command-card">
            <span>{t.commandPreview}</span>
            <code>{commandPreview}</code>
          </div>
          <div className="button-row">
            <button className="primary start-button" disabled={busy || running} onClick={() => runServiceCommand('start_service')}>
              <Play size={16} /> {t.start}
            </button>
            <button className="danger stop-button" disabled={busy || !running} onClick={() => runServiceCommand('stop_service')}>
              <CircleStop size={16} /> {t.stop}
            </button>
            <button className="secondary save-button" disabled={busy} onClick={() => save()}>
              <Save size={16} /> {t.save}
            </button>
            <button className="quiet refresh-button" disabled={busy} onClick={refresh}>
              <RefreshCw size={16} /> {t.refresh}
            </button>
          </div>
          {saveNotice && <div className={`save-feedback ${saveNotice}`}>{formatSaveNotice(t, saveNotice)}</div>}
        </Panel>


        <Panel className="logs-panel" title={t.logs} icon={<TerminalSquare size={18} />} action={
          <button className="ghost" onClick={() => runServiceCommand('clear_logs')}>{t.clear}</button>
        }>
          <div className="logs-box">
            {logs.length ? logs.map((line, index) => <div key={`${index}-${line}`}>{line}</div>) : <span className="muted">{t.noLogs}</span>}
          </div>
        </Panel>
      </section>

      {settingsOpen && (
        <SettingsModal
          t={t}
          tab={settingsTab}
          setTab={setSettingsTab}
          config={config}
          patchConfig={patchConfig}
          patchAndSave={patchAndSave}
          save={save}
          saveNotice={saveNotice}
          updateInfo={updateInfo}
          updateBusy={updateBusy}
          checkUpdates={checkUpdates}
          openReleases={openReleases}
          close={() => setSettingsOpen(false)}
        />
      )}
    </main>
  );
}

function SettingsModal({ t, tab, setTab, config, patchConfig, patchAndSave, save, saveNotice, updateInfo, updateBusy, checkUpdates, openReleases, close }) {
  return (
    <div className="modal-backdrop" onClick={close}>
      <section className="settings-modal" onClick={(event) => event.stopPropagation()}>
        <header className="modal-header">
          <div>
            <p className="eyebrow">Deskbridge</p>
            <h2>{t.settings}</h2>
          </div>
          <button className="icon-button light" onClick={close}><X size={18} /></button>
        </header>

        <div className="tabs">
          <button className={tab === 'general' ? 'active' : ''} onClick={() => setTab('general')}>
            <Settings size={16} /> {t.general}
          </button>
          <button className={tab === 'keyboard' ? 'active' : ''} onClick={() => setTab('keyboard')}>
            <Keyboard size={16} /> {t.keyboard}
          </button>
          <button className={tab === 'about' ? 'active' : ''} onClick={() => setTab('about')}>
            <Info size={16} /> {t.about}
          </button>
        </div>

        {tab === 'general' ? (
          <div className="settings-body">
            <h3>{t.general}</h3>
            {config ? (
              <>
                <label className="wide config-path-field">
                  {t.configPath}
                  <input
                    value={config.configPath || ''}
                    placeholder="/path/to/config.ini"
                    onChange={(event) => patchConfig({ configPath: event.target.value })}
                  />
                </label>
                <p className="settings-hint">{t.configPathHint}</p>
                <div className="save-line">
                  <button className="secondary" onClick={() => patchConfig({ configPath: '' })}>
                    {t.useDefaultConfigPath}
                  </button>
                  <button className="primary" onClick={() => save()} disabled={saveNotice === 'saving'}>
                    <Save size={16} /> {saveNotice === 'saving' ? t.saving : t.save}
                  </button>
                  {saveNotice && <span className={`save-feedback ${saveNotice}`}>{formatSaveNotice(t, saveNotice)}</span>}
                </div>
              </>
            ) : <Skeleton />}
          </div>
        ) : tab === 'keyboard' ? (
          <div className="settings-body">
            <h3>{t.activeMappings}</h3>
            <div className={`keyboard-scope-note ${config?.role === 'client' ? 'client-role' : ''}`}>
              <strong>{t.keyboardScopeTitle}</strong>
              <span>{t.keyboardScopeText}</span>
            </div>
            {config ? (
              <>
                <h3>{t.modifierMappings}</h3>
                <div className="form-grid compact">
                  <MappingSelect label="Command" value={config.macCommandMapping} options={mappingOptions} onChange={(value) => patchAndSave({ macCommandMapping: value })} />
                  <MappingSelect label="Control" value={config.macControlMapping} options={mappingOptions} onChange={(value) => patchAndSave({ macControlMapping: value })} />
                  <MappingSelect label="Option" value={config.macOptionMapping} options={mappingOptions} onChange={(value) => patchAndSave({ macOptionMapping: value })} />
                  <MappingSelect label="Shift" value={config.macShiftMapping} options={mappingOptions} onChange={(value) => patchAndSave({ macShiftMapping: value })} />
                </div>
                <h3>{t.specialMappings}</h3>
                <div className="form-grid compact key-map-grid">
                  <MappingSelect label="CapsLock" value={config.macCapsLockMapping} options={keyTargetOptions} onChange={(value) => patchAndSave({ macCapsLockMapping: value })} />
                  <MappingSelect label="Esc" value={config.macEscapeMapping} options={keyTargetOptions} onChange={(value) => patchAndSave({ macEscapeMapping: value })} />
                  <MappingSelect label="Backspace" value={config.macBackspaceMapping} options={keyTargetOptions} onChange={(value) => patchAndSave({ macBackspaceMapping: value })} />
                  <MappingSelect label="Delete" value={config.macDeleteMapping} options={keyTargetOptions} onChange={(value) => patchAndSave({ macDeleteMapping: value })} />
                  <MappingSelect label="Arrow Left" value={config.macArrowLeftMapping} options={keyTargetOptions} onChange={(value) => patchAndSave({ macArrowLeftMapping: value })} />
                  <MappingSelect label="Arrow Right" value={config.macArrowRightMapping} options={keyTargetOptions} onChange={(value) => patchAndSave({ macArrowRightMapping: value })} />
                  <MappingSelect label="Arrow Up" value={config.macArrowUpMapping} options={keyTargetOptions} onChange={(value) => patchAndSave({ macArrowUpMapping: value })} />
                  <MappingSelect label="Arrow Down" value={config.macArrowDownMapping} options={keyTargetOptions} onChange={(value) => patchAndSave({ macArrowDownMapping: value })} />
                </div>
              </>
            ) : <Skeleton />}
            <div className="save-line">
              <button className="primary" onClick={() => save()} disabled={saveNotice === 'saving'}>
                <Save size={16} /> {saveNotice === 'saving' ? t.saving : t.save}
              </button>
              {saveNotice && <span className={`save-feedback ${saveNotice}`}>{formatSaveNotice(t, saveNotice)}</span>}
            </div>
          </div>
        ) : (
          <div className="settings-body about-grid">
            <InfoRow label={t.version} value={APP_VERSION} />
            <InfoRow label={t.author} value={AUTHOR} />
            <InfoRow label={t.repo} value="paulhe666/deskbridge" />
            <div className="update-card">
              <div>
                <h3>{t.checkUpdates}</h3>
                <p>{t.updateHint}</p>
                <p className="update-result">{formatUpdateInfo(t, updateInfo)}</p>
              </div>
              <div className="update-actions">
                <button className="primary" disabled={updateBusy} onClick={checkUpdates}>
                  <RefreshCw size={16} /> {updateBusy ? t.loading : t.checkUpdates}
                </button>
                <button className="link-button" onClick={openReleases}>
                  <ExternalLink size={16} /> {t.manualUpdate}
                </button>
              </div>
            </div>
            <div className="update-card soft">
              <CheckCircle2 size={18} />
              <div>
                <h3>{t.autoUpdate}</h3>
                <p>{t.autoUpdateText}</p>
                {config && (
                  <label className="check-row">
                    <input
                      type="checkbox"
                      checked={Boolean(config.autoUpdateCheck)}
                      onChange={(event) => patchAndSave({ autoUpdateCheck: event.target.checked })}
                    />
                    {t.autoUpdate}
                  </label>
                )}
              </div>
            </div>
          </div>
        )}
      </section>
    </div>
  );
}

function formatSaveNotice(t, notice) {
  if (notice === 'saving') return `${t.saving}...`;
  if (notice === 'saved') return t.saved;
  if (notice === 'error') return t.saveFailed;
  return '';
}

function formatUpdateInfo(t, info) {
  if (!info) return t.updateUnknown;
  if (info.error) return info.error;
  if (info.hasUpdate) return `${t.updateAvailable}: v${info.latestVersion}`;
  return `${t.upToDate}: v${info.currentVersion}`;
}

function InfoRow({ label, value }) {
  return (
    <div className="info-row">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function Panel({ title, icon, children, action, className = '' }) {
  return (
    <section className={`panel ${className}`}>
      <header>
        <div className="panel-title">{icon}{title}</div>
        {action}
      </header>
      {children}
    </section>
  );
}

function Range({ label, value, min, max, step, onChange }) {
  return (
    <label className="range-row">
      <span>{label}</span>
      <input type="range" min={min} max={max} step={step} value={value} onChange={(event) => onChange(Number(event.target.value))} />
      <strong>{value}</strong>
    </label>
  );
}

function MappingSelect({ label, value, options, onChange }) {
  return (
    <label>
      {label}
      <select value={value} onChange={(event) => onChange(event.target.value)}>
        {options.map(([optionValue, optionLabel]) => (
          <option key={optionValue} value={optionValue}>{optionLabel}</option>
        ))}
      </select>
    </label>
  );
}

function Skeleton() {
  return <div className="skeleton">Loading configuration...</div>;
}

createRoot(document.getElementById('root')).render(<App />);
