import {useCallback, useEffect, useRef, useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {listen} from '@tauri-apps/api/event';
import Dialog from '@mui/material/Dialog';
import DialogActions from '@mui/material/DialogActions';
import DialogContent from '@mui/material/DialogContent';
import DialogTitle from '@mui/material/DialogTitle';
import ToolCard from './ToolCard';
import type {ApplicationView, UpdatePreview} from './types';
import './hub.css';

interface InstallChoice {appId: string; title: string; preview: UpdatePreview; python: string; running: boolean}

export default function HubApp() {
  const [apps, setApps] = useState<ApplicationView[]>([]);
  const [filter, setFilter] = useState<'all' | 'installed'>('all');
  const [query, setQuery] = useState('');
  const [loading, setLoading] = useState(true);
  const [globalError, setGlobalError] = useState('');
  const [loadError, setLoadError] = useState('');
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [choice, setChoice] = useState<InstallChoice | null>(null);
  const [log, setLog] = useState<{title: string; text: string} | null>(null);
  const inFlight = useRef(new Set<string>());
  const autoChecked = useRef(new Set<string>());

  const reload = useCallback(async () => {
    const data = await invoke<ApplicationView[]>('hub_list');
    setApps(data);
  }, []);

  useEffect(() => {
    let disposed = false;
    const refresh = async () => {
      try {
        const data = await invoke<ApplicationView[]>('hub_list');
        if (!disposed) {
          setApps(data); setLoadError('');
          for (const app of data) {
            const key = `${app.tool.id}:${app.installed_commit}`;
            if (!app.installed_commit || app.state_error || autoChecked.current.has(key)) continue;
            autoChecked.current.add(key);
            // Check once per installed revision. Network failure does not block launch
            // or replace the installed deployment; manual retry remains available.
            void invoke('hub_check_update', {appId: app.tool.id}).catch(() => {});
          }
        }
      } catch (error) {if (!disposed) setLoadError(String(error));}
      finally {if (!disposed) setLoading(false);}
    };
    void refresh();
    void invoke('show_main_window').catch(() => {});
    const interval = window.setInterval(() => void refresh(), 2000);
    const listener = listen<string>('hub-error', event => {if (!disposed) setGlobalError(event.payload);});
    return () => {disposed = true; window.clearInterval(interval); void listener.then(unlisten => unlisten()).catch(() => {});};
  }, []);

  const run = async (id: string, operation: () => Promise<void>) => {
    if (inFlight.current.has(id)) return;
    inFlight.current.add(id);
    setBusy(previous => ({...previous, [id]: true}));
    setErrors(previous => ({...previous, [id]: ''}));
    try {await operation(); await reload();}
    catch (error) {setErrors(previous => ({...previous, [id]: String(error)}));}
    finally {inFlight.current.delete(id); setBusy(previous => ({...previous, [id]: false}));}
  };

  const check = (app: ApplicationView) => void run(app.tool.id, async () => {
    const preview = await invoke<UpdatePreview>('hub_check_update', {appId: app.tool.id});
    setChoice({appId: app.tool.id, title: app.tool.title, preview, python: app.python_executable ?? '', running: app.running});
  });

  const install = () => {
    if (!choice || !choice.python.trim()) return;
    const selected = choice;
    setChoice(null);
    void run(selected.appId, async () => {
      await invoke('hub_install', {appId: selected.appId, targetCommit: selected.preview.target_commit, pythonExecutable: selected.python.trim()});
    });
  };

  const visible = apps.filter(app => (filter === 'all' || app.installed_commit) &&
    `${app.tool.title} ${app.tool.description}`.toLowerCase().includes(query.toLowerCase()));
  const installed = apps.filter(app => app.installed_commit).length;
  const running = apps.filter(app => app.running).length;

  return (
    <div className="hub-shell">
      <aside className="hub-sidebar">
        <div className="hub-brand"><span aria-hidden="true">↗</span><div>工具中心<small>ETG227 · PREVIEW</small></div></div>
        <nav aria-label="工具分类">
          <button className={filter === 'all' ? 'selected' : ''} aria-pressed={filter === 'all'} onClick={() => setFilter('all')}>所有工具 <span>{apps.length}</span></button>
          <button className={filter === 'installed' ? 'selected' : ''} aria-pressed={filter === 'installed'} onClick={() => setFilter('installed')}>已安装 <span>{installed}</span></button>
        </nav>
        <div className="sidebar-bottom"><p>基于 PyAppify</p><button onClick={() => void invoke('hub_shutdown').catch(error => setGlobalError(String(error)))}>停止工具并退出</button></div>
      </aside>
      <main className="hub-main">
        <header><div><p className="eyebrow">你的工具，一个入口</p><h1>{filter === 'all' ? '发现与使用' : '已安装的工具'}</h1><p>按需安装，独立运行。在这里管理每个工具的更新。</p></div><span className="running-summary" role="status">{running} 个工具运行中</span></header>
        <div className="preview-note">开发预览 · 当前安装需要已有 Python。运行时自动下载与正式更新通道仍在开发。</div>
        <label className="search-label">搜索工具<input type="search" placeholder="名称或用途" value={query} onChange={event => setQuery(event.target.value)} /></label>
        {(globalError || loadError) && <div className="global-error" role="alert">暂时无法完成操作。<details><summary>查看原因</summary>{globalError || loadError}</details><button onClick={() => void reload().then(() => {setGlobalError(''); setLoadError('');}).catch(error => setLoadError(String(error)))}>重试</button></div>}
        {loading ? <p role="status">正在读取工具目录…</p> : visible.length === 0 ? <p className="empty-state">{query ? '没有找到匹配的工具。' : filter === 'installed' ? '还没有安装工具，去“所有工具”看看。' : '工具目录暂时为空。'}</p> :
          <section className="tool-grid" aria-label="工具列表">{visible.map(app => <ToolCard key={app.tool.id} application={app} busy={Boolean(busy[app.tool.id] || app.busy)} error={errors[app.tool.id]} onCheck={() => check(app)} onAction={action => void run(app.tool.id, async () => {await invoke(`hub_${action}`, {appId: app.tool.id});})} onLog={() => void run(app.tool.id, async () => {const text = await invoke<string>('hub_read_log', {appId: app.tool.id}); setLog({title: app.tool.title, text});})} />)}</section>}
      </main>
      <Dialog open={choice !== null} onClose={() => setChoice(null)} fullWidth maxWidth="sm" aria-labelledby="update-title">
        <DialogTitle id="update-title">{choice?.title} · {choice?.preview.installed_commit ? '检查更新' : '安装工具'}</DialogTitle>
        <DialogContent>
          {choice && <>
            <p>{choice.preview.available ? `准备安装提交 ${choice.preview.target_commit.slice(0, 7)}` : '当前运行内容已是最新。'}</p>
            <p className="dialog-note">来自开发预览分支。此通道还没有正式版的检查门槛。</p>
            <ul className="commit-list">{choice.preview.commits.map((commit, index) => <li key={`${index}-${commit}`}>{commit}</li>)}</ul>
            {choice.preview.commit_count > 20 && <p>共 {choice.preview.commit_count} 个提交，仅显示最近20条。</p>}
            {choice.preview.available && <>
              <label className="python-label">Python 可执行文件<input value={choice.python} placeholder="例如 C:\Python312\python.exe" onChange={event => setChoice({...choice, python: event.target.value})} /></label>
              <p className="dialog-note">此工具需要 Python {choice.preview.python_version}{choice.preview.requires_tk ? ' 和 Tkinter' : ''}。工具中心会创建独立环境，不修改这份 Python 的依赖。</p>
              {choice.running && <p role="status">请先停止这个工具，再安装更新。</p>}
            </>}
          </>}
        </DialogContent>
        <DialogActions><button onClick={() => setChoice(null)}>关闭</button>{choice?.preview.available && <button className="primary" disabled={!choice.python.trim() || choice.running} onClick={install}>安装这个提交</button>}</DialogActions>
      </Dialog>
      <Dialog open={log !== null} onClose={() => setLog(null)} fullWidth maxWidth="md" aria-labelledby="log-title"><DialogTitle id="log-title">{log?.title} · 最近运行日志</DialogTitle><DialogContent><pre className="log-content">{log?.text || '还没有运行日志。'}</pre></DialogContent><DialogActions><button onClick={() => setLog(null)}>关闭</button></DialogActions></Dialog>
    </div>
  );
}
