import React, { useEffect, useMemo, useRef, useState } from 'react';
import { Check, ChevronDown, Search, X } from 'lucide-react';

export interface SelectOption {
  value: string;
  label?: string;
  badge?: string;
}

interface SearchableSelectProps {
  value: string;
  onChange: (nextValue: string) => void;
  options: (string | SelectOption)[];
  placeholder?: string;
  searchPlaceholder?: string;
  allowCustom?: boolean;
  className?: string;
}

export const SearchableSelect: React.FC<SearchableSelectProps> = ({
  value,
  onChange,
  options,
  placeholder = '请选择...',
  searchPlaceholder = '输入关键字快速筛选...',
  allowCustom = true,
  className = '',
}) => {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [highlightIdx, setHighlightIdx] = useState(0);
  const containerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const normalizedOptions: SelectOption[] = useMemo(() => {
    const seen = new Set<string>();
    const list: SelectOption[] = [];
    for (const item of options) {
      const opt: SelectOption =
        typeof item === 'string' ? { value: item, label: item } : item;
      if (!seen.has(opt.value)) {
        seen.add(opt.value);
        list.push(opt);
      }
    }
    if (value && !seen.has(value)) {
      list.unshift({ value, label: value, badge: '当前值' });
    }
    return list;
  }, [options, value]);

  const filteredOptions = useMemo(() => {
    const trimmed = query.trim().toLowerCase();
    if (!trimmed) return normalizedOptions;
    const tokens = trimmed.split(/\s+/);
    return normalizedOptions.filter((opt) => {
      const hay = `${opt.value} ${opt.label || ''} ${opt.badge || ''}`.toLowerCase();
      return tokens.every((t) => hay.includes(t));
    });
  }, [normalizedOptions, query]);

  const selectedOption = useMemo(
    () =>
      normalizedOptions.find((o) => o.value === value) || {
        value,
        label: value || placeholder,
      },
    [normalizedOptions, value, placeholder]
  );

  useEffect(() => {
    if (open) {
      setQuery('');
      const idx = normalizedOptions.findIndex((o) => o.value === value);
      setHighlightIdx(idx >= 0 ? idx : 0);
      requestAnimationFrame(() => {
        inputRef.current?.focus();
      });
    }
  }, [open, normalizedOptions, value]);

  useEffect(() => {
    setHighlightIdx(0);
  }, [query]);

  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    if (open) {
      document.addEventListener('mousedown', handleClickOutside);
    }
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, [open]);

  const commitValue = (nextVal: string) => {
    onChange(nextVal);
    setOpen(false);
    setQuery('');
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (!open) {
      if (e.key === 'Enter' || e.key === ' ' || e.key === 'ArrowDown') {
        e.preventDefault();
        setOpen(true);
      }
      return;
    }

    if (e.key === 'Escape') {
      e.preventDefault();
      setOpen(false);
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      setHighlightIdx((prev) =>
        filteredOptions.length > 0 ? (prev + 1) % filteredOptions.length : 0
      );
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setHighlightIdx((prev) =>
        filteredOptions.length > 0
          ? (prev - 1 + filteredOptions.length) % filteredOptions.length
          : 0
      );
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (filteredOptions[highlightIdx]) {
        commitValue(filteredOptions[highlightIdx].value);
      } else if (allowCustom && query.trim()) {
        commitValue(query.trim());
      }
    }
  };

  return (
    <div ref={containerRef} className={`relative ${className}`} onKeyDown={handleKeyDown}>
      <button
        type="button"
        onClick={() => setOpen((prev) => !prev)}
        className={`w-full flex items-center justify-between gap-2 px-3 py-2 rounded-xl bg-[#070a10] border text-xs text-left transition cursor-pointer ${
          open
            ? 'border-indigo-500/80 ring-2 ring-indigo-500/20 text-white'
            : 'border-slate-800 hover:border-slate-700 text-slate-100'
        }`}
      >
        <span className="truncate font-mono">{selectedOption.label || selectedOption.value}</span>
        <span className="flex items-center gap-1.5 shrink-0">
          {normalizedOptions.length > 5 && (
            <span className="text-[10px] font-mono px-1.5 py-0.2 rounded bg-slate-800/90 text-slate-400">
              {normalizedOptions.length}
            </span>
          )}
          <ChevronDown
            className={`w-3.5 h-3.5 text-slate-400 transition-transform ${
              open ? 'rotate-180 text-indigo-400' : ''
            }`}
          />
        </span>
      </button>

      {open && (
        <div className="absolute left-0 right-0 mt-1.5 z-50 rounded-xl border border-slate-700/90 bg-[#0b101b] shadow-2xl shadow-black/80 overflow-hidden">
          {/* Search Input Header */}
          <div className="p-2 border-b border-slate-800/90 bg-[#080c14] flex items-center gap-1.5">
            <Search className="w-3.5 h-3.5 text-indigo-400 ml-1.5 shrink-0" />
            <input
              ref={inputRef}
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={searchPlaceholder}
              className="w-full bg-transparent text-xs text-slate-100 placeholder-slate-500 focus:outline-none font-mono py-0.5"
            />
            {query && (
              <button
                type="button"
                onClick={() => {
                  setQuery('');
                  inputRef.current?.focus();
                }}
                className="p-0.5 rounded hover:bg-slate-800 text-slate-400 hover:text-slate-200 cursor-pointer"
              >
                <X className="w-3 h-3" />
              </button>
            )}
            <span className="text-[10px] font-mono text-slate-500 shrink-0 pr-1">
              {filteredOptions.length}/{normalizedOptions.length}
            </span>
          </div>

          {/* Options List */}
          <div ref={listRef} className="max-h-64 overflow-y-auto p-1 divide-y divide-slate-800/30">
            {filteredOptions.length === 0 ? (
              <div className="p-3 text-center space-y-2">
                <div className="text-xs text-slate-400">无匹配项</div>
                {allowCustom && query.trim() && (
                  <button
                    type="button"
                    onClick={() => commitValue(query.trim())}
                    className="w-full px-2.5 py-1.5 rounded-lg bg-indigo-600/20 hover:bg-indigo-600/30 border border-indigo-500/40 text-indigo-300 text-xs font-mono cursor-pointer"
                  >
                    使用自定义值: &quot;{query.trim()}&quot;
                  </button>
                )}
              </div>
            ) : (
              filteredOptions.map((opt, idx) => {
                const isSelected = opt.value === value;
                const isHighlighted = idx === highlightIdx;
                return (
                  <button
                    key={opt.value}
                    type="button"
                    onMouseEnter={() => setHighlightIdx(idx)}
                    onClick={() => commitValue(opt.value)}
                    className={`w-full flex items-center justify-between gap-2 px-2.5 py-1.5 rounded-lg text-xs text-left transition cursor-pointer ${
                      isSelected
                        ? 'bg-indigo-600/20 text-indigo-200 font-medium'
                        : isHighlighted
                        ? 'bg-slate-800/80 text-slate-100'
                        : 'text-slate-300 hover:bg-slate-800/50'
                    }`}
                  >
                    <span className="truncate font-mono">{opt.label || opt.value}</span>
                    <span className="flex items-center gap-1.5 shrink-0">
                      {opt.badge && (
                        <span className="text-[10px] font-mono px-1.5 py-0.2 rounded bg-slate-800 text-slate-400">
                          {opt.badge}
                        </span>
                      )}
                      {isSelected && <Check className="w-3.5 h-3.5 text-indigo-400" />}
                    </span>
                  </button>
                );
              })
            )}
          </div>
        </div>
      )}
    </div>
  );
};
