<script lang="ts">
import {
	Cloud,
	CloudDrizzle,
	CloudFog,
	CloudHail,
	CloudLightning,
	CloudRain,
	CloudSnow,
	CloudSun,
	Droplets,
	MapPin,
	Snowflake,
	Sun,
	Thermometer,
	Wind,
} from "lucide-svelte";
import { hideWindow, openUri } from "$lib/ipc";

interface CurrentWeather {
	temp: number;
	humidity: number | null;
	wind_speed: number | null;
	condition: string;
	symbol: string;
}

interface DayForecast {
	day: string;
	temp_high: number;
	temp_low: number;
	symbol: string;
	condition: string;
}

interface WeatherData {
	location: string;
	unit: string;
	current: CurrentWeather;
	forecast: DayForecast[];
}

let { data }: { data: WeatherData } = $props();

const iconMap: Record<string, typeof Sun> = {
	clearsky: Sun,
	fair: CloudSun,
	partlycloudy: CloudSun,
	cloudy: Cloud,
	fog: CloudFog,
	lightrain: CloudDrizzle,
	rain: CloudRain,
	heavyrain: CloudRain,
	lightrainshowers: CloudDrizzle,
	rainshowers: CloudRain,
	heavyrainshowers: CloudRain,
	lightrainandthunder: CloudLightning,
	rainandthunder: CloudLightning,
	heavyrainandthunder: CloudLightning,
	lightrainshowersandthunder: CloudLightning,
	rainshowersandthunder: CloudLightning,
	heavyrainshowersandthunder: CloudLightning,
	lightsleet: CloudHail,
	sleet: CloudHail,
	heavysleet: CloudHail,
	lightsleetshowers: CloudHail,
	sleetshowers: CloudHail,
	heavysleetshowers: CloudHail,
	lightsnow: CloudSnow,
	snow: Snowflake,
	heavysnow: Snowflake,
	lightsnowshowers: CloudSnow,
	snowshowers: Snowflake,
	heavysnowshowers: Snowflake,
};

function getIcon(symbol: string) {
	return iconMap[symbol] ?? Thermometer;
}

let CurrentIcon = $derived(getIcon(data.current.symbol));

async function openDetailedReport() {
	const url = `https://www.accuweather.com/en/search-locations?query=${encodeURIComponent(data.location)}`;
	await hideWindow();
	await openUri(url);
}
</script>

<div class="weather-card">
	<div class="current">
		<div class="current-icon">
			<CurrentIcon size={36} strokeWidth={1.5} />
		</div>
		<div class="current-info">
			<div class="temp-row">
				<span class="temp">{data.current.temp}&deg;{data.unit}</span>
				<span class="condition">{data.current.condition}</span>
			</div>
			<div class="details">
				{#if data.current.humidity != null}
					<span class="detail">
						<Droplets size={12} strokeWidth={1.5} />
						{data.current.humidity.toFixed(0)}%
					</span>
				{/if}
				{#if data.current.wind_speed != null}
					<span class="detail">
						<Wind size={12} strokeWidth={1.5} />
						{data.current.wind_speed.toFixed(0)} m/s
					</span>
				{/if}
			</div>
			<div class="location">
				<MapPin size={10} strokeWidth={1.5} />
				{data.location}
			</div>
		</div>
	</div>

	{#if data.forecast.length > 0}
		<div class="forecast">
			{#each data.forecast as day}
				{@const DayIcon = getIcon(day.symbol)}
				<div class="forecast-day">
					<span class="day-name">{day.day}</span>
					<DayIcon size={16} strokeWidth={1.5} />
					<span class="day-temps">
						<span class="high">{day.temp_high}&deg;</span>
						<span class="low">{day.temp_low}&deg;</span>
					</span>
				</div>
			{/each}
		</div>
	{/if}

	<div class="weather-actions">
		<button class="btn-detail" onclick={openDetailedReport}>
			Details
		</button>
	</div>
</div>

<style>
	.weather-card {
		padding: 16px 20px;
		font-family: var(--font-sans, system-ui, -apple-system, sans-serif);
		color: var(--fg);
	}

	.current {
		display: flex;
		gap: 16px;
		align-items: flex-start;
	}

	.current-icon {
		flex-shrink: 0;
		color: var(--fg);
		opacity: 0.85;
		padding-top: 2px;
	}

	.current-info {
		display: flex;
		flex-direction: column;
		gap: 4px;
		min-width: 0;
		flex: 1;
	}

	.temp-row {
		display: flex;
		align-items: baseline;
		gap: 10px;
	}

	.temp {
		font-family: var(--font-mono);
		font-size: 24px;
		font-weight: 600;
		line-height: 1;
	}

	.condition {
		font-size: 14px;
		color: var(--fg-muted);
		text-transform: capitalize;
	}

	.details {
		display: flex;
		gap: 14px;
		font-size: 12px;
		color: var(--fg-muted);
	}

	.detail {
		display: flex;
		align-items: center;
		gap: 4px;
	}

	.location {
		display: flex;
		align-items: center;
		gap: 4px;
		font-size: 11px;
		color: var(--fg-muted);
		opacity: 0.7;
		margin-top: 2px;
	}

	.forecast {
		display: flex;
		gap: 2px;
		margin-top: 14px;
		padding-top: 12px;
		border-top: 1px solid var(--border);
	}

	.forecast-day {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 6px;
		padding: 8px 4px;
		border-radius: 6px;
		border: 1px solid var(--border);
		color: var(--fg-muted);
	}

	.day-name {
		font-family: var(--font-mono);
		font-size: 11px;
		font-weight: 600;
		color: var(--fg);
		text-transform: uppercase;
		letter-spacing: 0.5px;
	}

	.day-temps {
		font-family: var(--font-mono);
		font-size: 12px;
		display: flex;
		gap: 6px;
	}

	.high {
		color: var(--fg);
		font-weight: 600;
	}

	.low {
		color: var(--fg-muted);
	}

	.weather-actions {
		display: flex;
		justify-content: flex-end;
		margin-top: 12px;
		padding-top: 10px;
		border-top: 1px solid var(--border);
	}

	.btn-detail {
		font-family: var(--font-mono);
		font-size: 11px;
		padding: 5px 14px;
		border-radius: 4px;
		border: 1px solid var(--accent);
		cursor: pointer;
		display: flex;
		align-items: center;
		gap: 6px;
		transition: all 100ms ease;
		background: var(--accent);
		color: var(--bg);
	}

	.btn-detail:hover {
		opacity: 0.85;
	}
</style>
