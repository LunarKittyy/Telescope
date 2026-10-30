<p align="center"><img src="docs/assets/banner.png" alt="Telescope: используй телефон как веб-камеру с любым из его объективов и полным ручным управлением. Приложение на компьютере принимает видео с телефона, рядом показано приложение на телефоне." width="720"></p>

<p align="center"><a href="README.md">English</a> · <a href="README.es.md">Español</a> · Русский</p>

<p align="center">
  <a href="../../releases/latest"><img src="https://img.shields.io/endpoint?url=https://telescope.webcam/release.json" alt="Версия"></a>
  <a href="../../releases"><img src="https://img.shields.io/endpoint?url=https://telescope.webcam/downloads.json" alt="Скачивания"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-orange" alt="Лицензия"></a>
  <a href="https://ko-fi.com/lunarkittyy"><img src="https://img.shields.io/badge/Ko--fi-support-FF5E5B?logo=ko-fi&logoColor=white" alt="Ko-fi"></a>
</p>

<p align="center">Telescope превращает твой телефон на Android в веб-камеру для компьютера с Linux или Windows.<br>Можно использовать все его объективы, от сверхширокоугольного до телеобъектива. Всё бесплатно, без водяных знаков, рекламы и аккаунта.</p>

<p align="center">
  <a href="https://github.com/LunarKittyy/Telescope/releases/latest/download/TelescopeSetup.exe"><img src="docs/assets/download-windows.svg" alt="Скачать для Windows" height="56"></a>
  <a href="https://github.com/LunarKittyy/Telescope/releases/latest/download/Telescope-linux.tar.gz"><img src="docs/assets/download-linux.svg" alt="Скачать для Linux" height="56"></a>
  <a href="https://github.com/LunarKittyy/Telescope/releases/latest/download/Telescope.apk"><img src="docs/assets/download-android.svg" alt="Скачать для Android" height="56"></a>
</p>

## Всё готово примерно за две минуты

<p align="center"><img src="site/media/setup.avif" alt="Список шагов в приложении на компьютере, сканирование кода сопряжения телефоном и выбор Telescope в качестве камеры для видеозвонка" width="800"></p>

1. **Скачай приложение для компьютера.** В Windows запусти `TelescopeSetup.exe` (оно устанавливается только для твоей учётной записи и не требует прав администратора). В Linux распакуй архив и запусти `./start.sh`.
2. **Следуй инструкциям, которые появятся при запуске.** Они помогут настроить виртуальную камеру, установить приложение на телефон с помощью QR-кода и связать оба приложения.
3. **Выбери камеру в своём приложении.** В Discord, Zoom, OBS и любом другом приложении с поддержкой веб-камер ищи **Telescope** в Windows или **Phone Camera** в Linux.

<details>
<summary>Что нужно для Linux</summary>

- **Python 3.12 или новее**
- **`v4l2loopback`** из твоего дистрибутива для виртуальной камеры (`sudo apt install v4l2loopback-dkms` в Debian и Ubuntu, `sudo dnf install v4l2loopback` в Fedora с [RPM Fusion](https://rpmfusion.org/))
- **`adb`** нужен только для USB. По Wi-Fi всё работает без него

Остальные подробности есть в [инструкции по ручной настройке](docs/setup.md#linux), в том числе о старой версии Python в Ubuntu 22.04.
</details>

Нет телефона на Android? [Камера в браузере](docs/features.md#browser-camera) передаёт видео с iPhone, планшета или старого ноутбука прямо из браузера, без установки приложений.

## Больше чем веб-камера

Подключай телефоны по USB или Wi-Fi и используй до четырёх сразу, каждый как отдельную веб-камеру. Микрофон телефона тоже можно использовать на компьютере.

<table>
  <tr>
    <td width="33%"><img src="site/media/pro.avif" alt="Ручная настройка ISO, выдержки, фокуса и баланса белого из приложения на компьютере"><br><b>Профессиональные настройки</b><br>Настраивай ISO, выдержку, фокус и баланс белого вручную, как в режиме Pro в приложении камеры на телефоне.</td>
    <td width="33%"><img src="site/media/framing.avif" alt="Автокадрирование удерживает человека в кадре"><br><b>Автокадрирование</b><br>Меняет масштаб и сдвигает изображение, чтобы ты оставался в кадре.</td>
    <td width="33%"><img src="site/media/chroma.avif" alt="Вместо зелёного фона подставлена картинка"><br><b>Фон и хромакей</b><br>Размой фон или замени его, с зелёным экраном или без него.</td>
  </tr>
  <tr>
    <td><img src="site/media/one-picture.avif" alt="Две камеры объединены в одну картинку"><br><b>Несколько камер в одной картинке</b><br>Рядом, сеткой или в собственной раскладке, для приложений, которые работают только с одной камерой.</td>
    <td><img src="site/media/desk.avif" alt="Лист бумаги на столе с исправленной перспективой, как при съёмке сверху"><br><b>Съёмка стола</b><br>Исправляет перспективу плоских предметов в кадре, например страницы или рисунка, чтобы собеседники видели их без наклона.</td>
    <td><img src="site/media/fun.avif" alt="Ретроэффекты VHS и Game Boy на изображении с камеры"><br><b>Набор развлечений</b><br>Вечеринка клонов, плащ-невидимка, VHS, веб-камера из 1998 года и многое другое.</td>
  </tr>
</table>

Большинство этих функций доступны в виде плагинов, которые ты включаешь, когда они нужны. Есть ещё цветокоррекция, размытие для приватности, плашка с именем, заметки рядом с объективом и другие плагины. Можно устанавливать и плагины других авторов.

В документации описано [всё, что умеет Telescope](docs/features.md), а ещё есть [сравнение](https://telescope.webcam/compare.html) с DroidCam, Iriun и Camo.

## Частые вопросы

<details>
<summary><b>Это правда бесплатно?</b></summary>

Да. Все функции, любое разрешение видео, без водяных знаков и рекламы. Если всё равно хочешь поддержать проект, есть [Ko-fi](https://ko-fi.com/lunarkittyy).
</details>

<details>
<summary><b>Моё видео проходит через чей-то сервер?</b></summary>

Нет. Оно передаётся напрямую с телефона на твой компьютер по USB или по твоей сети и шифруется с помощью TLS. Здесь нет аккаунтов и облака. Приложения выходят в интернет только для проверки обновлений на GitHub или чтобы скачать adb у Google или VB-Cable у VB-Audio, когда ты об этом попросишь. Используй только сеть, которой доверяешь, или включи **Только локально - USB** в приложении на Android. Подробности в разделе [«Приватность»](docs/features.md#privacy).
</details>

<details>
<summary><b>Будет работать с Discord / Zoom / Teams / OBS / моим браузером?</b></summary>

Если приложение или браузер умеет работать с веб-камерой, то да. Telescope отображается как обычная камера, и ты выбираешь её так же, как любую другую.
</details>

<details>
<summary><b>У меня iPhone или Mac.</b></summary>

Приложение для телефона доступно только на Android, но [камера в браузере](docs/features.md#browser-camera) передаёт видео из браузера iPhone без установки приложений. Приложение для компьютера работает на Windows и Linux, но не на macOS.
</details>

<details>
<summary><b>На моём телефоне заработает?</b></summary>

Нужен Android 8 или новее. В разделе [«Совместимость устройств»](docs/device-compatibility.md) перечислены проверенные модели. Если попробуешь на телефоне, которого там нет, [расскажи, как всё прошло](https://github.com/LunarKittyy/Telescope/discussions/new?category=device-compatibility). Это одна из самых полезных вещей, которые ты можешь сделать для проекта.
</details>

<details>
<summary><b>Не хочешь ничего устанавливать в Windows?</b></summary>

В `Telescope-windows-portable.zip` на [странице релизов](../../releases) лежит то же приложение. Распакуй архив и запусти `TelescopeDesktop.exe`, не перемещая его из папки. Папка `lib-...` рядом с ним тоже нужна для работы приложения.
</details>

<details>
<summary><b>Это сделано с помощью ИИ?</b></summary>

Да, я многое делаю в этом проекте с помощью ИИ-агентов. Код генерирует ИИ, но я сам решаю, что делает Telescope и как он должен работать. Проверкой на реальном оборудовании тоже в основном занимаюсь я (не считая автоматических тестов).
</details>

Что-то не получается или есть идея? Спроси в [Discussions](../../discussions). Нашёл ошибку? Создай [сообщение об ошибке](https://github.com/LunarKittyy/Telescope/issues/new?template=bug.yml) и приложи диагностический отчёт. Его можно скопировать кнопкой **«Скопировать диагностику»**. Частые проблемы разобраны в разделе [«Устранение неполадок»](docs/troubleshooting.md).

## Как помочь

Буду рад рассказам о том, как Telescope работает на твоих устройствах, сообщениям об ошибках с диагностикой и пул-реквестам. Подробности в [CONTRIBUTING.md](CONTRIBUTING.md), а [документация](docs/README.md) объясняет, как всё устроено.

Telescope распространяется по лицензии [GNU Affero General Public License v3.0](LICENSE). Раздел [«Лицензии»](docs/licenses.md) объясняет, что это значит для тебя, и перечисляет используемые сторонние компоненты.

<a href="https://www.star-history.com/?repos=LunarKittyy%2FTelescope&type=date&releases=&legend=top-left">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=LunarKittyy/Telescope&type=date&theme=dark&releases=&legend=top-left" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=LunarKittyy/Telescope&type=date&releases=&legend=top-left" />
    <img alt="График количества звёзд на GitHub" src="https://api.star-history.com/chart?repos=LunarKittyy/Telescope&type=date&releases=&legend=top-left" />
  </picture>
</a>
