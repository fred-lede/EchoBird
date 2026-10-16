import { ExternalLink } from 'lucide-react';
import { useI18n } from '../../hooks/useI18n';
import { useToast } from '../../components/Toast';
import { useAiCareerStore } from '../../stores/aiCareerStore';
import { useToolsStore } from '../../stores/toolsStore';
import { openExternal } from '../../api/tauri';
import { CAREER_FAMILIES, FamilyIcon } from './familyIcons';

export function AiCareerFamilyPanel() {
  const { t } = useI18n();
  const { showToast } = useToast();
  const tools = useToolsStore((s) => s.detectedTools);
  const selectedFamily = useAiCareerStore((s) => s.selectedFamily);
  const setSelectedFamily = useAiCareerStore((s) => s.setSelectedFamily);
  const cardClass = (selected: boolean) =>
    'w-full min-h-16 flex border border-transparent rounded transition-colors ' +
    (selected ? 'bg-cyber-elevated' : 'bg-cyber-surface hover:bg-cyber-elevated');
  const openWebsite = async (url: string) => {
    try {
      await openExternal(url);
    } catch {
      showToast('error', t('aiCareer.actionFailed'));
    }
  };

  return (
    <nav
      aria-label={t('aiCareer.families')}
      className="flex-1 min-h-0 px-2 pb-2 overflow-y-auto slim-scroll space-y-2 cursor-default"
    >
      <button
        type="button"
        onClick={() => setSelectedFamily(null)}
        className={cardClass(false) + ' items-center justify-center px-3 text-center'}
      >
        <span className="text-base font-bold text-cyber-text truncate">
          {t('aiCareer.overview')}
        </span>
      </button>
      {CAREER_FAMILIES.map((family) => {
        const website = tools.find((tool) => tool.id === family.toolId)?.website;
        return (
          <div key={family.id} className={cardClass(selectedFamily === family.id)}>
            <button
              type="button"
              aria-pressed={selectedFamily === family.id}
              onClick={() => setSelectedFamily(family.id)}
              className="flex-1 min-w-0 flex items-center gap-3 px-3 py-2 text-left"
            >
              <span
                aria-hidden="true"
                className={`w-[16px] h-[16px] flex-shrink-0 rounded-full border-2 flex items-center justify-center ${
                  selectedFamily === family.id ? 'border-cyber-accent' : 'border-cyber-border'
                }`}
              >
                {selectedFamily === family.id && (
                  <span className="w-[8px] h-[8px] rounded-full bg-cyber-accent" />
                )}
              </span>
              <FamilyIcon family={family.id} />
              <span className="text-base font-bold text-cyber-text truncate">
                {family.name} {t('aiCareer.familySuffix')}
              </span>
            </button>
            {website && (
              <button
                type="button"
                aria-label={t('accountCenter.website').replace('{name}', family.name)}
                onClick={() => void openWebsite(website)}
                className="flex-shrink-0 px-3 text-cyber-text-muted hover:text-cyber-text"
              >
                <ExternalLink size={20} />
              </button>
            )}
          </div>
        );
      })}
    </nav>
  );
}
