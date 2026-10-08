// SPDX-FileCopyrightText: 2026 cybersader
// SPDX-License-Identifier: GPL-3.0-only
// Opt-in local native witness, NOT a release acceptance gate or publishing command.
import { createHash } from "node:crypto";
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { By, Key, until } from "selenium-webdriver";
import { validateProductionIdentity } from "./production-package.mjs";
import {
  attachWebdriver, isolatedEnvironment,
  launchPinnedObsidian, preparePrivateLayout, prepareVerifiedRuntime,
  reapProcess, validateRuntimeManifest, verifyPortsClosed, waitForCdpPort,
} from "./webdriver-release-gate.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const OUTPUT = resolve(process.env.KWIRY_HOVER_OUTPUT ?? resolve(ROOT, ".tmp/hover-native/evidence"));
const QUERY = "cobalt lantern";
const FILE = "Orchard Atlas.md";
const TOP = "WHOLE FILE TOP: the ceramic heron rests beside a turquoise sundial.";
const BOTTOM = "WHOLE FILE BOTTOM: seven paper kites cross the violet orchard.";
const evidence = { kind: "local_native_hover_witness", fixture: { query: QUERY, file: FILE, top: TOP, bottom: BOTTOM }, cases: [], cleanup: {} };
let driver, proc, privateRoot, mainHandle;
const oldEnv = { ...process.env };
const ports = [];
await mkdir(OUTPUT, { recursive: true, mode: 0o700 });
const flush = async () => writeFile(resolve(OUTPUT, "observations.json"), `${JSON.stringify(evidence, null, 2)}\n`);
const replaceEnv = (env) => { for (const key of Object.keys(process.env)) delete process.env[key]; Object.assign(process.env, env); };
const snapshot = () => driver.executeScript(`
  const s = window.__kwiryHoverWitness;
  return {
    query: document.querySelector('.prompt-input')?.value ?? null,
    selected: Array.from(document.querySelectorAll('.kwiry-result.is-selected')).map(e => e.textContent),
    rows: Array.from(document.querySelectorAll('.kwiry-result')).map(e => ({text:e.textContent, kind:e.classList.contains('kwiry-section-result')?'section':'source'})),
    popups: Array.from(document.querySelectorAll('.hover-popover')).map(e => {
      const r=e.getBoundingClientRect(), x=r.x+r.width/2,y=r.y+r.height/2;
      const hit=document.elementFromPoint(x,y);
      const rect={x:r.x,y:r.y,width:r.width,height:r.height};
      const exposed=[];
      for(let py=r.y+15;py<Math.min(r.bottom,innerHeight)-10;py+=20){
        const px=r.x+r.width/2;
        if(e.contains(document.elementFromPoint(px,py)))exposed.push({x:px,y:py});
      }
      return {text:e.textContent, visible:r.width>0, rect, zIndex:getComputedStyle(e).zIndex,
        centerHit:hit?.className,centerInPopup:e.contains(hit),exposed,html:e.outerHTML.slice(0,24000)};
    }),
    modalGeometry: (()=>{const e=document.querySelector('.modal-container');if(!e)return null;const r=e.getBoundingClientRect();return {zIndex:getComputedStyle(e).zIndex,backgroundZIndex:getComputedStyle(document.querySelector('.modal-bg')).zIndex,rect:{x:r.x,y:r.y,width:r.width,height:r.height}};})(),
    pointerEvents: s?.pointerEvents ?? [],
    mainFiles: app.workspace.getLeavesOfType('markdown').filter(l => !l.containerEl.closest('.hover-popover')).map(l => l.view.file?.path ?? null),
    searchCalls: s?.searchCalls ?? null, openCalls: s?.openCalls ?? null,
    hoverEvents: s?.hoverEvents ?? [], popoverAssignments:s?.popoverAssignments??[], notices:Array.from(document.querySelectorAll('.notice')).map(e=>e.textContent),
    modal: Boolean(document.querySelector('.kwiry-result')),
  };`);
async function record(name, before, expectedPopup, extra = {}) {
  const after = await snapshot();
  const popup = after.popups.find(p => p.visible && p.text.includes(TOP) && p.text.includes(BOTTOM));
  const unchanged = before.query === after.query
    && (extra.expectedSelectionChange || JSON.stringify(before.selected) === JSON.stringify(after.selected))
    && before.searchCalls === after.searchCalls && before.openCalls === after.openCalls
    && JSON.stringify(before.mainFiles) === JSON.stringify(after.mainFiles);
  const passed = (expectedPopup ? Boolean(popup) : after.popups.length === 0) && unchanged;
  evidence.cases.push({ name, expectedPopup, passed, unchanged, before, after, ...extra });
  await writeFile(resolve(OUTPUT, `${name}.png`), Buffer.from(await driver.takeScreenshot(), "base64"));
  await flush();
  console.log(JSON.stringify({ name, passed, popup: Boolean(popup), unchanged }));
  return after;
}
async function away() {
  await driver.actions().keyUp(Key.CONTROL).move({ x: 20, y: 20, origin: 'viewport' }).perform();
  await driver.sleep(650);
}
async function row(kind = "source") { return driver.wait(until.elementLocated(By.css(`.kwiry-${kind}-result`)), 15000); }
async function enter(kind = "source", ctrl = true) {
  await away();
  const target = await row(kind);
  const before = await snapshot();
  const actions = driver.actions();
  if (ctrl) actions.keyDown(Key.CONTROL);
  await actions.move({ origin: target }).perform();
  await driver.sleep(1700);
  return before;
}
async function openSearch() {
  await away();
  await driver.actions().keyDown(Key.CONTROL).sendKeys("p").keyUp(Key.CONTROL).perform();
  const input = await driver.wait(until.elementLocated(By.css(".prompt-input")), 15000);
  await input.sendKeys("Kwiry Search: Search notes");
  await driver.wait(until.elementLocated(By.css(".suggestion-item")), 15000).then(e => e.click());
  const query = await driver.wait(until.elementLocated(By.css(".prompt-input")), 15000);
  await query.sendKeys(QUERY);
  await row();
  await driver.sleep(800);
}
async function switchSettings() {
  await driver.sleep(600);
  const handles=await driver.getAllWindowHandles();
  evidence.settingsWindowCount=handles.length;
  const settings=handles.find(h=>h!==mainHandle);
  if(settings)await driver.switchTo().window(settings);
  await driver.sleep(600);
  evidence.settingsWindowTitle=await driver.getTitle();
}
async function closeSettings() {
  const handle=await driver.getWindowHandle();
  await driver.actions().sendKeys(Key.ESCAPE).perform();
  await driver.sleep(500);
  if(handle!==mainHandle){
    if((await driver.getAllWindowHandles()).includes(handle)){
      const closeButtons=await driver.findElements(By.css('.titlebar-button.mod-close'));
      if(closeButtons.length)await closeButtons[0].click();
      else await driver.close();
    }
    await driver.switchTo().window(mainHandle);
  }
  await driver.sleep(300);
}
async function instrument() {
  // Observation only: real backend searches/openFile calls are counted and passed
  // through unchanged. Native hover-link dispatch is observed, never manufactured.
  await driver.executeAsyncScript(`
    const done=arguments[arguments.length-1];
    (async()=>{
      const p=app.plugins.plugins['kwiry-search'];
      const backend=await p.backendManager.current();
      const s=window.__kwiryHoverWitness={searchCalls:0,openCalls:0,hoverEvents:[],pointerEvents:[],popoverAssignments:[]};
      const observedParents=new WeakSet();
      document.addEventListener('mouseout', e=>{
        if(!e.target?.closest?.('.kwiry-result'))return;
        s.pointerEvents.push({type:e.type,trusted:e.isTrusted,target:e.target.className,related:e.relatedTarget?.className,popupRelated:Boolean(e.relatedTarget?.closest?.('.hover-popover')),x:e.clientX,y:e.clientY});
      },true);
      const search=backend.search;
      backend.search=function(...args){s.searchCalls++;return search.apply(this,args);};
      for(const leaf of app.workspace.getLeavesOfType('markdown')){
        const open=leaf.openFile;
        leaf.openFile=function(...args){s.openCalls++;return open.apply(this,args);};
      }
      app.workspace.on('hover-link', data=>{
        if(data.source!==p.hoverPreviewSource)return;
        // Transparent fixture-only observation of the owned public parent field.
        // It preserves values and does not style/create/remove native UI.
        if(!observedParents.has(data.hoverParent)){
          const parent=data.hoverParent;
          observedParents.add(parent);
          let object=parent, original;
          while(object && !original){original=Object.getOwnPropertyDescriptor(object,'hoverPopover');object=Object.getPrototypeOf(object);}
          let value=parent.hoverPopover;
          Object.defineProperty(parent,'hoverPopover',{configurable:true,enumerable:true,get(){return original?.get?original.get.call(parent):value;},set(next){
            // Preserve the production setter, including ownership/disposal guards.
            if(original?.set)original.set.call(parent,next);else value=next;
            const assigned=parent.hoverPopover;
            s.popoverAssignments.push({assigned:Boolean(assigned),connected:Boolean(assigned?.hoverEl?.isConnected),state:assigned?.state??null,elementClass:assigned?.hoverEl?.className??null});
          }});
        }
        const event={source:data.source,linktext:data.linktext,sourcePath:data.sourcePath,ctrl:data.event?.ctrlKey,trusted:data.event?.isTrusted,type:data.event?.type,popoverTiming:[]};
        s.hoverEvents.push(event);
        const observe=(phase)=>{
          const p=data.hoverParent?.hoverPopover;
          event.popoverTiming.push({phase,assigned:Boolean(p),connected:Boolean(p?.hoverEl?.isConnected),state:p?.state??null,elementClass:p?.hoverEl?.className??null});
        };
        observe('workspace-listener');
        queueMicrotask(()=>observe('microtask'));
        for(const delay of [20,300,1000])setTimeout(()=>observe(delay+'ms'),delay);
      });
      return {source:p.hoverPreviewSource,pagePreviewEnabled:app.internalPlugins.plugins['page-preview']?.enabled};
    })().then(done,e=>done({error:String(e)}));`)
    .then(value => { evidence.instrumentation = value; });
}
try {
  const manifest = validateRuntimeManifest(JSON.parse(await readFile(resolve(ROOT, "scripts/webdriver-release-gate-manifest.json"))), JSON.parse(await readFile(resolve(ROOT, "package.json"))));
  // Electron SingletonSocket has a Unix path-length limit. Keep state under a
  // short owned disk-backed /tmp root; large pinned assets remain in .tmp cache.
  privateRoot = await mkdtemp(resolve(tmpdir(), "kh-"));
  const layout = await preparePrivateLayout(privateRoot);
  // Pinned setup rechecks every cached artifact and both derived binaries.
  replaceEnv(isolatedEnvironment(layout, layout.tmp));
  await prepareVerifiedRuntime(layout, manifest, {});
  const pluginDir = resolve(layout.vault, ".obsidian/plugins/kwiry-search");
  await mkdir(pluginDir, { recursive: true });
  const identity=await validateProductionIdentity(ROOT);
  evidence.candidate = { version: identity.manifest.version, identityMirrorsValidated:true, releasePackageAcceptance:false, runtimeFiles: {} };
  for (const name of ["main.js", "manifest.json", "styles.css"]) {
    await copyFile(resolve(ROOT, name), resolve(pluginDir, name));
    evidence.candidate.runtimeFiles[name] = createHash("sha256").update(await readFile(resolve(ROOT, name))).digest("hex");
  }
  await writeFile(resolve(pluginDir, "data.json"), JSON.stringify({ backendProfile: "in_plugin", defaultMode: "lexical", resultLimit: 20, showRibbonIcon: true, diagnosticsLogLevel: "off", diagnosticsReportLevel: "error", diagnosticsReportScope: "failures" }));
  await writeFile(resolve(layout.vault, ".obsidian/community-plugins.json"), JSON.stringify(["kwiry-search"]));
  // Fixture-only initial core-plugin setup; modifier preferences use host defaults.
  await writeFile(resolve(layout.vault, ".obsidian/core-plugins.json"), JSON.stringify(["file-explorer", "global-search", "switcher", "backlink", "outgoing-link", "page-preview", "command-palette", "editor-status"]));
  await writeFile(resolve(layout.vault, FILE), `# Orchard Atlas\n\n${TOP}\n\n## North pergola\n\n${QUERY} illuminates the painted gazebo.\n\n## South alcove\n\n${QUERY} marks the woven basket.\n\n${BOTTOM}\n`);
  evidence.environmentPrepared = true;
  await flush();
  console.log('ENVIRONMENT_PREPARED');
  const launched = await launchPinnedObsidian({ layout, manifest });
  proc = launched.proc;
  const cdpPort = await waitForCdpPort(launched.configDir, proc); ports.push(cdpPort);
  const attached = await attachWebdriver({ layout, manifest, cdpPort }); driver = attached.driver; ports.push(attached.webdriverPort);
  mainHandle = await driver.getWindowHandle();
  await driver.manage().setTimeouts({ script: 30000, implicit: 0, pageLoad: 30000 });
  try { await driver.manage().window().setRect({width:1600,height:1000}); } catch {}
  await driver.wait(async () => driver.executeScript("return Boolean(app?.plugins?.enabledPlugins?.has('kwiry-search') && app.commands.commands['kwiry-search:open-search']);"), 90000);
  await driver.wait(async () => driver.executeAsyncScript(`const done=arguments[arguments.length-1];(async()=>{const b=await app.plugins.plugins['kwiry-search'].backendManager.current();return Boolean((await b.status()).searchable)})().then(done,()=>done(false));`), 90000);
  evidence.runtime = await driver.executeScript("return {electron:process.versions.electron,chromium:process.versions.chrome,embeddedNode:process.versions.node,versionGlobals:Object.keys(window).filter(k=>/version/i.test(k)).map(k=>({key:k,value:typeof window[k]==='string'?window[k]:typeof window[k]}))};");
  evidence.runtime.obsidian = manifest.runtime.obsidian_app;
  evidence.runtime.obsidianIdentity = 'SHA-256 verified installer and app ASAR';
  evidence.runtime.launcherNode = process.versions.node;
  evidence.runtime.driver = (await driver.getCapabilities()).get("chrome")?.chromedriverVersion?.split(" ")[0];
  evidence.runtime.launcher = manifest.dependencies.obsidian_launcher;
  evidence.runtime.selenium = manifest.dependencies.selenium_webdriver;
  await instrument();
  await openSearch();
  let before = await enter("source", false);
  await record("source-default-no-ctrl", before, false);
  before = await snapshot();
  await driver.actions().keyDown(Key.CONTROL).perform();
  await driver.sleep(1700);
  await record("source-ctrl-after-stationary", before, true);
  before = await enter();
  await record("source-ctrl-before-entry", before, true);
  const popup = (await driver.findElements(By.css(".hover-popover")))[0];
  if (popup) {
    before = await snapshot();
    evidence.cases.push({name:'popup-stacking-hit-test',passed:before.popups[0].centerInPopup,observation:before.popups[0],modal:before.modalGeometry});
    await driver.actions().move({origin:popup}).perform();
    await driver.sleep(800);
    await record("pointer-into-popup-center", before, true);
    before = await enter();
    const visibleState = await snapshot();
    const point = visibleState.popups[0]?.exposed?.[0];
    if(point){
      before=visibleState;
      await driver.actions().move({x:Math.round(point.x),y:Math.round(point.y),origin:'viewport'}).perform();
      await driver.sleep(800);
      await record('pointer-into-exposed-popup',before,true,{point});
    } else evidence.cases.push({name:'pointer-into-exposed-popup',blocked:'no exposed popup hit point'});
  } else evidence.cases.push({name:"pointer-into-popup-center",blocked:"source popup absent"});
  await away();
  await driver.actions().keyDown(Key.CONTROL).sendKeys("l").keyUp(Key.CONTROL).perform();
  await row("section"); await driver.sleep(500);
  before = await enter("section", false);
  await record('section-default-no-ctrl',before,false);
  before=await snapshot();
  await driver.actions().keyDown(Key.CONTROL).perform();await driver.sleep(1700);
  await record('section-ctrl-after-stationary',before,true);
  before = await enter("section", true);
  await record("section-ctrl-before-entry", before, true);
  // Native local navigation must dismiss, without another backend query/open.
  before=await snapshot();
  await driver.actions().sendKeys('h').keyUp(Key.CONTROL).perform();
  await row('source');await driver.sleep(1700);
  await record('section-to-source-rerender-dismissal',before,false,{expectedSelectionChange:true});
  before=await enter('source',true);await record('source-popup-before-rerender',before,true);
  before=await snapshot();
  await driver.actions().sendKeys('l').keyUp(Key.CONTROL).perform();
  await row('section');await driver.sleep(1700);
  await record('source-to-section-rerender-dismissal',before,false,{expectedSelectionChange:true});
  before=await enter('section',true);await record('section-popup-before-query-change',before,true);
  before=await snapshot();
  await driver.actions().keyUp(Key.CONTROL).perform();
  const input=await driver.findElement(By.css('.prompt-input'));
  await input.sendKeys(' quartz');await driver.sleep(1700);
  const queryAfter=await snapshot();
  evidence.cases.push({name:'query-change-dismissal',passed:before.popups.length>0 && queryAfter.popups.length===0 && queryAfter.query===QUERY+' quartz' && queryAfter.openCalls===before.openCalls, before,after:queryAfter});
  await driver.actions().sendKeys(Key.ESCAPE).perform();
  await openSearch();
  before=await enter();await record('source-popup-before-modal-close',before,true);
  before=await snapshot();await driver.actions().keyUp(Key.CONTROL).sendKeys(Key.ESCAPE).perform();await driver.sleep(1700);
  const closed=await snapshot();
  evidence.cases.push({name:'modal-close-dismissal',passed:closed.popups.length===0 && !closed.modal && closed.openCalls===before.openCalls && closed.searchCalls===before.searchCalls,before,after:closed});
  // Lifecycle fixture actions are explicit API calls, not pretend user inputs.
  // They invoke the real product/host lifecycle and do not synthesize hover DOM.
  await openSearch();before=await enter();await record('source-popup-before-backend-invalidation',before,true);
  before=await snapshot();
  evidence.backendInvalidationAction=await driver.executeAsyncScript(`const done=arguments[arguments.length-1];(async()=>{const p=app.plugins.plugins['kwiry-search'];const old=p.getActiveBackendIdentity()?.instanceId;await p.activateBackendProfile();return {changed:old!==p.getActiveBackendIdentity()?.instanceId,fixtureAction:'real activateBackendProfile'};})().then(done,e=>done({error:String(e)}));`);
  await driver.sleep(1700);
  evidence.backendShellAt1700=await snapshot();
  await driver.sleep(1800);
  const invalidated=await snapshot();
  await writeFile(resolve(OUTPUT,'backend-invalidation-stationary-3500ms.png'),Buffer.from(await driver.takeScreenshot(),'base64'));
  evidence.cases.push({name:'backend-invalidation-dismissal',passed:evidence.backendInvalidationAction.changed && invalidated.popups.length===0 && invalidated.query===before.query && invalidated.openCalls===before.openCalls,before,after:invalidated,fixtureLifecycleAction:true});
  await driver.actions().keyUp(Key.CONTROL).sendKeys(Key.ESCAPE).perform();
  await instrument();await openSearch();before=await enter();await record('source-popup-before-plugin-unload',before,true);
  before=await snapshot();
  evidence.unloadAction=await driver.executeAsyncScript(`const done=arguments[arguments.length-1];(async()=>{await app.plugins.unloadPlugin('kwiry-search');return {unloaded:!app.plugins.plugins['kwiry-search'],fixtureAction:'native PluginManager unloadPlugin'};})().then(done,e=>done({error:String(e)}));`);
  await driver.sleep(1700);
  evidence.unloadShellAt1700=await snapshot();
  await driver.sleep(1800);
  const unloaded=await snapshot();
  await writeFile(resolve(OUTPUT,'plugin-unload-stationary-3500ms.png'),Buffer.from(await driver.takeScreenshot(),'base64'));
  evidence.cases.push({name:'plugin-unload-dismissal',passed:evidence.unloadAction.unloaded && unloaded.popups.length===0 && unloaded.openCalls===before.openCalls,before,after:unloaded,fixtureLifecycleAction:true});
  await driver.actions().keyUp(Key.CONTROL).sendKeys(Key.ESCAPE).perform();
  await driver.executeAsyncScript(`const done=arguments[arguments.length-1];app.plugins.loadPlugin('kwiry-search').then(()=>done(true),e=>done({error:String(e)}));`);
  await driver.wait(async()=>driver.executeAsyncScript(`const done=arguments[arguments.length-1];(async()=>{const b=await app.plugins.plugins['kwiry-search'].backendManager.current();return (await b.status()).searchable;})().then(done,()=>done(false));`),30000);
  await instrument();
  // Open native settings through a genuine command-palette selection.
  await away();
  await driver.actions().keyDown(Key.CONTROL).sendKeys('p').keyUp(Key.CONTROL).perform();
  const settingsInput=await driver.wait(until.elementLocated(By.css('.prompt-input')),15000);
  await settingsInput.sendKeys('Open settings');
  const settingsCommand=await driver.wait(until.elementLocated(By.css('.suggestion-item')),15000);
  evidence.settingsCommandText=await settingsCommand.getText();
  await settingsCommand.click();await switchSettings();
  evidence.settingsUI = await driver.executeScript("return {tabs:Array.from(document.querySelectorAll('.vertical-tab-nav-item')).map(e=>e.textContent),html:document.querySelector('.mod-settings')?.outerHTML.slice(0,30000)};");
  const tab=(await driver.findElements(By.xpath("//*[contains(@class,'vertical-tab-nav-item') and normalize-space(.)='Page preview']")))[0];
  if(tab){
    await tab.click();await driver.sleep(400);
    evidence.pagePreviewSettings=await driver.executeScript("return Array.from(document.querySelectorAll('.setting-item')).map(e=>({text:e.textContent,html:e.outerHTML}));");
    await writeFile(resolve(OUTPUT,'native-page-preview-settings.png'),Buffer.from(await driver.takeScreenshot(),'base64'));
    const sourceSetting=(await driver.findElements(By.xpath("//*[contains(concat(' ',normalize-space(@class),' '),' setting-item ') and .//*[contains(@class,'setting-item-name') and normalize-space(.)='Kwiry Search']]")))[0];
    if(sourceSetting){
      const toggle=await sourceSetting.findElement(By.css('.checkbox-container'));
      evidence.preferenceBefore={className:await toggle.getAttribute('class'),checked:await toggle.findElement(By.css('input')).isSelected()};
      await toggle.click();await driver.sleep(400);
      evidence.preferenceAfter={className:await toggle.getAttribute('class'),checked:await toggle.findElement(By.css('input')).isSelected()};
      await writeFile(resolve(OUTPUT,'native-preference-toggled.png'),Buffer.from(await driver.takeScreenshot(),'base64'));
      await closeSettings();
      await openSearch();before=await enter('source',false);
      await record('native-preference-source-no-ctrl',before,true,{nativePreferenceUI:true});
      await away();await driver.actions().keyDown(Key.CONTROL).sendKeys('l').keyUp(Key.CONTROL).perform();await row('section');
      before=await enter('section',false);
      await record('native-preference-section-no-ctrl',before,true,{nativePreferenceUI:true});
      await away();await driver.actions().sendKeys(Key.ESCAPE).perform();
    } else evidence.cases.push({name:'native-preference-no-ctrl',blocked:'Kwiry Search native toggle not found'});
  } else evidence.cases.push({name:'native-preference-no-ctrl',blocked:'native Page preview settings tab not found'});
  // Reopen native settings and disable the core plugin using its real UI.
  if(await driver.getWindowHandle()!==mainHandle || await driver.findElements(By.css('.mod-settings')).then(es=>es.length))await closeSettings();
  await away();await driver.actions().keyDown(Key.CONTROL).sendKeys('p').keyUp(Key.CONTROL).perform();
  await driver.wait(until.elementLocated(By.css('.prompt-input')),15000).then(e=>e.sendKeys('Open settings'));
  await driver.wait(until.elementLocated(By.css('.suggestion-item')),15000).then(e=>e.click());await switchSettings();
  const coreTab=(await driver.findElements(By.xpath("//*[contains(@class,'vertical-tab-nav-item') and normalize-space(.)='Core plugins']")))[0];
  if(coreTab){
    await coreTab.click();await driver.sleep(400);
    evidence.corePluginSettings=await driver.executeScript("return Array.from(document.querySelectorAll('.setting-item')).filter(e=>e.textContent.includes('Page preview')).map(e=>({text:e.textContent,html:e.outerHTML}));");
    const pageRow=(await driver.findElements(By.xpath("//*[contains(concat(' ',normalize-space(@class),' '),' setting-item ') and .//*[contains(@class,'setting-item-name') and normalize-space(.)='Page preview']]")))[0];
    if(pageRow){
      const coreToggle=await pageRow.findElement(By.css('.checkbox-container'));
      evidence.corePreviewBefore={className:await coreToggle.getAttribute('class'),checked:await coreToggle.findElement(By.css('input')).isSelected()};
      await coreToggle.click();await driver.sleep(400);
      // The native core-plugin list rerenders on toggle; observe the new node.
      evidence.corePreviewAfter=await driver.executeScript("const name=Array.from(document.querySelectorAll('.setting-item-name')).find(e=>e.textContent==='Page preview');const row=name?.closest('.setting-item');return {className:row?.querySelector('.checkbox-container')?.className,checked:row?.querySelector('input')?.checked};");
      await closeSettings();
      evidence.disabledPreviewObserved=await driver.executeScript("return app.internalPlugins.plugins['page-preview'].enabled===false;");
      await openSearch();before=await enter('source',true);
      await record('page-preview-disabled-source',before,false,{disabledViaNativeUI:true});
      await away();await driver.actions().keyDown(Key.CONTROL).sendKeys('l').keyUp(Key.CONTROL).perform();await row('section');
      before=await enter('section',true);
      await record('page-preview-disabled-section',before,false,{disabledViaNativeUI:true});
      // Real searches still returned both sections while Page Preview was off.
      evidence.disabledSearchRows=(await snapshot()).rows.length;
      await away();await driver.actions().sendKeys(Key.ESCAPE).perform();
    } else evidence.cases.push({name:'page-preview-disabled',blocked:'native core plugin toggle not found'});
  } else evidence.cases.push({name:'page-preview-disabled',blocked:'native core plugin settings tab not found'});
  await flush();
} catch (error) {
  evidence.failure = {name:error.name, message:error.message};
  console.error(JSON.stringify(evidence.failure));
  process.exitCode = 1;
} finally {
  try { if(driver)await driver.quit(); evidence.cleanup.webdriverQuit=true; } catch { evidence.cleanup.webdriverQuit=false; }
  try { if(proc)await reapProcess(proc); evidence.cleanup.obsidianReaped=!proc || proc.exitCode!==null; } catch { evidence.cleanup.obsidianReaped=false; }
  evidence.cleanup.portsClosed=await verifyPortsClosed(ports);
  replaceEnv(oldEnv);
  try { if(privateRoot)await rm(privateRoot,{recursive:true,force:true}); evidence.cleanup.privateStateRemoved=true; } catch { evidence.cleanup.privateStateRemoved=false; }
  evidence.summary = {
    passed: evidence.cases.filter(c=>c.passed===true).length,
    failed: evidence.cases.filter(c=>c.passed===false).length,
    blocked: evidence.cases.filter(c=>c.blocked).length,
  };
  evidence.verdict = evidence.failure ? 'host_execution_failed' : evidence.summary.failed ? 'native_regression_reproduced' : evidence.summary.blocked ? 'incomplete' : 'passed';
  if(evidence.failure || evidence.summary.failed || evidence.summary.blocked)process.exitCode=1;
  await flush();
  console.log(JSON.stringify({verdict:evidence.verdict,...evidence.summary,cleanup:evidence.cleanup}));
}
