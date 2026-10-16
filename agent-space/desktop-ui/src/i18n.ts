import i18next from 'i18next';
import zh from './locales/zh-CN.json';
import en from './locales/en.json';

export function t(key: string, options?: Record<string,unknown>): string { return i18next.t(key,{seconds:60,...options}) as string; }
export function formatBytes(bytes: number): string {
  const index=Math.min(3,Math.max(0,Math.floor(Math.log2(Math.max(1,bytes))/10)));
  return new Intl.NumberFormat(i18next.language,{maximumFractionDigits:index ? 1 : 0}).format(bytes/(1024**index))+' '+['B','KiB','MiB','GiB'][index];
}
function apply(): void {
  document.documentElement.lang=i18next.language;
  document.title=t('runcove_cloud_desktop');
  for(const element of document.querySelectorAll<HTMLElement>('[data-i18n]')) {
    element.textContent=t(element.dataset.i18n!);
  }
  for(const attribute of ['title','placeholder','aria-label']) {
    for(const element of document.querySelectorAll<HTMLElement>(`[data-i18n-${attribute}]`)) {
      element.setAttribute(attribute,t(element.getAttribute(`data-i18n-${attribute}`)!));
    }
  }
  const select=document.querySelector<HTMLSelectElement>('#language');
  if(select)select.value=i18next.language;
}
export async function initialize(): Promise<void> {
  let saved: string|null=null;
  try {saved=localStorage.getItem('runcove.desktop.language');} catch {}
  const language=['zh-CN','en'].includes(saved ?? '') ? saved! :
    navigator.languages.find(value=>/^en|^zh/i.test(value))?.toLowerCase().startsWith('en') ? 'en' : 'zh-CN';
  await i18next.init({lng:language,fallbackLng:'en',resources:{'zh-CN':{translation:zh},en:{translation:en}},interpolation:{escapeValue:false}});
  apply();
  document.querySelector<HTMLSelectElement>('#language')!.addEventListener('change',async event=>{
    const value=(event.target as HTMLSelectElement).value;
    await i18next.changeLanguage(value);
    try {localStorage.setItem('runcove.desktop.language',value);} catch {}
    apply();window.dispatchEvent(new Event('localechange'));
  });
}
