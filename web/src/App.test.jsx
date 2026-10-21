// @vitest-environment jsdom
import React from 'react';
import {test,expect,vi,afterEach} from 'vitest';
import {render,screen,cleanup,waitFor} from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import {App} from './App.jsx';
afterEach(cleanup);
const view={platform:'macos',state:'pairing',devices:[{id:'a'.repeat(32),name:'Windows',paired:false}],selectedId:'a'.repeat(32),pairingCode:null,secondsLeft:0,error:null,autoConnect:true,imageClipboard:true,fileClipboard:true,logs:[],appVersion:'0.4.0'};
test('polling does not erase the pairing code being typed',async()=>{
 const api=vi.fn(async()=>({...view}));render(<App api={api} refreshMs={25}/>);
 const input=await screen.findByLabelText('配对码');await userEvent.type(input,'001234');
 await waitFor(()=>expect(api.mock.calls.filter(c=>c[0]==='get_connect_state').length).toBeGreaterThan(2));
 expect(input.value).toBe('001234');
});
test('a failed pairing action remains readable across background refreshes',async()=>{
 const api=vi.fn(async name=>{if(name==='pair_device')throw new Error('配对码已失效');return {...view};});render(<App api={api} refreshMs={25}/>);
 await userEvent.type(await screen.findByLabelText('配对码'),'123456');await userEvent.click(screen.getByRole('button',{name:'配对并连接'}));
 expect(await screen.findByRole('alert')).toBeTruthy();
 await waitFor(()=>expect(api.mock.calls.filter(c=>c[0]==='get_connect_state').length).toBeGreaterThan(3));
 expect(screen.getByRole('alert').textContent).toContain('配对码已失效');
});
test('a settings action failure stays visible inside the open dialog',async()=>{
 const api=vi.fn(async name=>{if(name==='set_options')throw new Error('设置文件不可写');return {...view,state:'paused'};});render(<App api={api} refreshMs={25}/>);
 await screen.findByLabelText('配对码');await userEvent.click(screen.getByRole('button',{name:'高级设置'}));
 await userEvent.click(screen.getByRole('button',{name:'保存设置'}));
 await waitFor(()=>expect(screen.getByRole('dialog').textContent).toContain('设置文件不可写'));
});
