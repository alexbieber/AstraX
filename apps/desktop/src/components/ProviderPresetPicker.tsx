import { useId } from "react";
import type { ReactNode } from "react";
import { Check, SlidersHorizontal } from "lucide-react";
import "../styles/provider-preset-picker.css";

export type ProviderPresetBrand = "custom" | "openai" | "deepseek" | "minimax" | "mimo" | "qwen" | "kimi" | "glm" | "doubao";

export type ProviderPresetChoice = {
  id: string;
  name: string;
  nameEn?: string;
  brand: ProviderPresetBrand;
  subtitle?: string;
  subtitleEn?: string;
};

function PresetMark({ brand }: { brand: ProviderPresetBrand }) {
  const logo = brand === "openai" ? "/openai.svg" : `/provider-logos/${brand}.svg`;
  return <span className={`cx-preset-mark cx-preset-mark--${brand}`} aria-hidden="true">
    {brand === "custom" ? <SlidersHorizontal size={19} strokeWidth={1.8} /> : brand === "doubao" ? "豆" : <span className="cx-preset-brand-logo" style={{ WebkitMaskImage: `url("${logo}")`, maskImage: `url("${logo}")` }} />}
  </span>;
}

export function ProviderPresetPicker({ lang, presets, selectedId, disabled = false, onSelect, children }: {
  lang: "zh" | "en";
  presets: readonly ProviderPresetChoice[];
  selectedId: string;
  disabled?: boolean;
  onSelect: (id: string) => void;
  children?: ReactNode;
}) {
  const id = useId();
  const title = lang === "zh" ? "预设供应商" : "Provider presets";

  return <section className="cx-preset-picker" aria-label={title}>
    <fieldset className="cx-preset-fieldset" disabled={disabled} aria-label={title}>
      <div className="cx-preset-grid">
        {presets.map((preset) => {
          const name = lang === "en" ? preset.nameEn ?? preset.name : preset.name;
          return <label className="cx-preset-option" key={preset.id}>
            <input className="cx-preset-radio" type="radio" name={`${id}-provider-preset`} value={preset.id} checked={preset.id === selectedId} onChange={() => onSelect(preset.id)} aria-label={name} />
            <span className="cx-preset-card">
              <PresetMark brand={preset.brand} />
              <span className="cx-preset-copy"><strong>{name}</strong></span>
              <span className="cx-preset-selected" aria-hidden="true"><Check size={13} strokeWidth={2.8} /></span>
            </span>
          </label>;
        })}
      </div>
    </fieldset>
    {children && <div className="cx-preset-details">{children}</div>}
  </section>;
}
