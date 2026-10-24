import React from 'react';
import {createRoot} from 'react-dom/client';
import {App} from './App.jsx';
import {EdgeDrop} from './EdgeDrop.jsx';
import './style.css';
const root=createRoot(document.getElementById('root'));
const surface=new URLSearchParams(location.search).get('surface');
if(surface){
 document.body.classList.add('drop-window');const kind=surface==='receive'?'receive':'edge';
 if(import.meta.env.DEV&&new URLSearchParams(location.search).has('preview')){
  document.body.classList.add('drop-preview',kind==='receive'?'drop-preview-receive':'drop-preview-edge');if(new URLSearchParams(location.search).get('preview')==='edge')document.body.classList.add('drop-preview-outgoing');
  import('./preview.js').then(({createDropPreviewBoundary})=>{const boundary=createDropPreviewBoundary(new URLSearchParams(location.search).get('preview'));root.render(<EdgeDrop kind={kind} api={boundary.api} events={boundary.events}/>);});
 }else root.render(<EdgeDrop kind={kind}/>);
}
else if(import.meta.env.DEV&&new URLSearchParams(location.search).has('preview')){
 import('./preview.js').then(({createPreviewApi})=>root.render(<App preview api={createPreviewApi(new URLSearchParams(location.search).get('preview'))}/>));
}else root.render(<App/>);
